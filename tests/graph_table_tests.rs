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

//! Comprehensive tests for GraphAdjacency table engine.

use nanostore::table::{GraphAdjacency, GraphConfig, MemoryGraphTable};
use nanostore::txn::TransactionId;
use nanostore::types::TableId;
use nanostore::wal::LogSequenceNumber;

/// Helper to commit graph changes for tests
fn commit_graph(graph: &MemoryGraphTable) {
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(1))
        .unwrap();
}

#[test]
fn test_graph_basic_operations() {
    let config = GraphConfig::new();
    let graph = MemoryGraphTable::new(TableId::from(1), "test_graph".to_string(), config);

    // Add edges
    graph
        .add_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"bob",
            b"follows",
            b"charlie",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"follows",
            b"charlie",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    // Query outgoing edges
    let cursor = graph.outgoing(b"alice", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 2);

    // Query incoming edges
    let cursor = graph.incoming(b"charlie", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 2);

    // Remove an edge
    graph
        .remove_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let cursor = graph.outgoing(b"alice", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_graph_directed() {
    let config = GraphConfig::new().with_directed(true);
    let mut graph = MemoryGraphTable::new(TableId::from(1), "directed_graph".to_string(), config);

    // Add directed edge
    graph
        .add_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    // Alice follows Bob
    let cursor = graph.outgoing(b"alice", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);

    // Bob doesn't follow Alice (directed)
    let cursor = graph.outgoing(b"bob", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 0);

    // Bob has one incoming edge
    let cursor = graph.incoming(b"bob", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_graph_undirected() {
    let config = GraphConfig::new().with_directed(false);
    let mut graph = MemoryGraphTable::new(TableId::from(1), "undirected_graph".to_string(), config);

    // Add undirected edge
    graph
        .add_edge(
            b"alice",
            b"friends",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    // Both directions should work
    let cursor = graph.outgoing(b"alice", Some(b"friends")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);

    let cursor = graph.outgoing(b"bob", Some(b"friends")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_graph_multiple_labels() {
    let config = GraphConfig::new();
    let mut graph =
        MemoryGraphTable::new(TableId::from(1), "multi_label_graph".to_string(), config);

    // Add edges with different labels
    graph
        .add_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"likes",
            b"bob",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"follows",
            b"charlie",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    // Query by specific label
    let cursor = graph.outgoing(b"alice", Some(b"follows")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 2);

    let cursor = graph.outgoing(b"alice", Some(b"likes")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);

    // Query all labels
    let cursor = graph.outgoing(b"alice", None).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 3);
}

#[test]
fn test_graph_bfs_traversal() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "bfs_graph".to_string(), config);

    // Create a simple graph: alice -> bob -> charlie -> dave
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"bob",
            b"knows",
            b"charlie",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"charlie",
            b"knows",
            b"dave",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"eve",
            b"edge4",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let mut visited = Vec::new();
    graph
        .bfs(b"alice", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    // Should visit all reachable vertices
    assert!(visited.len() >= 4);
    assert_eq!(visited[0], b"alice");
}

#[test]
fn test_graph_dfs_traversal() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "dfs_graph".to_string(), config);

    // Create a simple graph
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"bob",
            b"knows",
            b"charlie",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"charlie",
            b"knows",
            b"dave",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let mut visited = Vec::new();
    graph
        .dfs(b"alice", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    // Should visit all reachable vertices
    assert!(visited.len() >= 3);
    assert_eq!(visited[0], b"alice");
}

#[test]
fn test_graph_neighbors() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "neighbors_graph".to_string(), config);

    graph
        .add_edge(
            b"alice",
            b"knows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"charlie",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"dave",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let neighbors = graph.neighbors(b"alice").unwrap();
    assert_eq!(neighbors.len(), 3);
    assert!(neighbors.contains(&b"bob".to_vec()));
    assert!(neighbors.contains(&b"charlie".to_vec()));
    assert!(neighbors.contains(&b"dave".to_vec()));
}

#[test]
fn test_graph_has_edge() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "has_edge_graph".to_string(), config);

    graph
        .add_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    assert!(graph.has_edge(b"alice", b"bob", Some(b"follows")).unwrap());
    assert!(!graph.has_edge(b"bob", b"alice", Some(b"follows")).unwrap());
    assert!(
        !graph
            .has_edge(b"alice", b"charlie", Some(b"follows"))
            .unwrap()
    );
}

#[test]
fn test_graph_cycle_detection() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "cycle_graph".to_string(), config);

    // Create a cycle: alice -> bob -> charlie -> alice
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"bob",
            b"knows",
            b"charlie",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"charlie",
            b"knows",
            b"alice",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let mut visited = Vec::new();
    graph
        .bfs(b"alice", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    // Should handle cycles correctly (visit each vertex once)
    assert_eq!(visited.len(), 3);
}

#[test]
fn test_graph_disconnected_components() {
    let config = GraphConfig::new();
    let mut graph =
        MemoryGraphTable::new(TableId::from(1), "disconnected_graph".to_string(), config);

    // Component 1: alice -> bob
    graph
        .add_edge(
            b"alice",
            b"knows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    // Component 2: charlie -> dave
    graph
        .add_edge(
            b"charlie",
            b"knows",
            b"dave",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    // BFS from alice should only visit component 1
    let mut visited = Vec::new();
    graph
        .bfs(b"alice", |vertex| {
            visited.push(vertex.to_vec());
            true
        })
        .unwrap();

    assert_eq!(visited.len(), 2);
    assert!(visited.contains(&b"alice".to_vec()));
    assert!(visited.contains(&b"bob".to_vec()));
    assert!(!visited.contains(&b"charlie".to_vec()));
}

#[test]
fn test_graph_stats() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "stats_graph".to_string(), config);

    graph
        .add_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"bob",
            b"follows",
            b"charlie",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let stats = graph.stats().unwrap();
    assert!(stats.entry_count.is_some());
    assert!(stats.entry_count.unwrap() >= 2);
}

#[test]
fn test_graph_empty() {
    let config = GraphConfig::new();
    let graph = MemoryGraphTable::new(TableId::from(1), "empty_graph".to_string(), config);

    let cursor = graph.outgoing(b"alice", None).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 0);
}

#[test]
fn test_graph_self_loop() {
    let config = GraphConfig::new();
    let mut graph = MemoryGraphTable::new(TableId::from(1), "self_loop_graph".to_string(), config);

    // Add self-loop
    graph
        .add_edge(
            b"alice",
            b"likes",
            b"alice",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let cursor = graph.outgoing(b"alice", Some(b"likes")).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source.0, b"alice");
    assert_eq!(edges[0].target.0, b"alice");
}

#[test]
fn test_graph_parallel_edges() {
    let config = GraphConfig::new();
    let mut graph =
        MemoryGraphTable::new(TableId::from(1), "parallel_edges_graph".to_string(), config);

    // Add multiple edges between same vertices with different labels
    graph
        .add_edge(
            b"alice",
            b"follows",
            b"bob",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"likes",
            b"bob",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"alice",
            b"mentions",
            b"bob",
            b"edge3",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    commit_graph(&graph);

    let cursor = graph.outgoing(b"alice", None).unwrap();
    let edges: Vec<_> = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 3);
}

// Made with Bob
