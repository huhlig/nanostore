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

//! Tests for MVCC support in MemoryGraphTable

use nanostore::table::GraphAdjacency;
use nanostore::table::graph::{GraphConfig, MemoryGraphTable};
use nanostore::txn::{TransactionId, VersionChain};
use nanostore::types::TableId;
use nanostore::wal::LogSequenceNumber;

#[test]
fn test_graph_mvcc_uncommitted_edges_not_visible() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    // Add an edge but don't commit it
    graph
        .add_edge(
            b"node1",
            b"follows",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    // Edge should not be visible yet (uncommitted)
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 0, "Uncommitted edge should not be visible");
}

#[test]
fn test_graph_mvcc_committed_edges_visible() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    // Add an edge
    graph
        .add_edge(
            b"node1",
            b"follows",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    // Commit the edge
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    // Edge should now be visible
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1, "Committed edge should be visible");
    assert_eq!(edges[0].source.0, b"node1");
    assert_eq!(edges[0].target.0, b"node2");
    assert_eq!(edges[0].label.0, b"follows");
}

#[test]
fn test_graph_mvcc_edge_removal_creates_tombstone() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    // Add and commit an edge
    graph
        .add_edge(
            b"node1",
            b"follows",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    // Verify edge is visible
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);

    // Remove the edge
    graph
        .remove_edge(
            b"node1",
            b"follows",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();

    // Commit the removal
    graph
        .commit_versions(TransactionId::from(2), LogSequenceNumber::from(20))
        .unwrap();

    // Edge should no longer be visible
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 0, "Removed edge should not be visible");
}

#[test]
fn test_graph_mvcc_multiple_versions() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    // Add first version
    graph
        .add_edge_with_weight(
            b"node1",
            b"likes",
            b"node2",
            b"edge1",
            Some(1.0),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    // Verify first version
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);

    // Add second version (update weight by removing and re-adding)
    graph
        .remove_edge(
            b"node1",
            b"likes",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge_with_weight(
            b"node1",
            b"likes",
            b"node2",
            b"edge1",
            Some(2.0),
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .commit_versions(TransactionId::from(2), LogSequenceNumber::from(20))
        .unwrap();

    // Should see the new version
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_graph_mvcc_incoming_edges() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    // Add edges
    graph
        .add_edge(
            b"node1",
            b"follows",
            b"node3",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"node2",
            b"follows",
            b"node3",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    // Check incoming edges to node3
    let cursor = graph.incoming(b"node3", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 2, "Should have 2 incoming edges");
}

#[test]
fn test_graph_mvcc_label_filtering() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    // Add edges with different labels
    graph
        .add_edge(
            b"node1",
            b"follows",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .add_edge(
            b"node1",
            b"likes",
            b"node3",
            b"edge2",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    // Filter by "follows" label
    let cursor = graph.outgoing(b"node1", Some(b"follows")).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].label.0, b"follows");

    // Filter by "likes" label
    let cursor = graph.outgoing(b"node1", Some(b"likes")).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].label.0, b"likes");

    // No filter - should get both
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 2);
}

#[test]
fn test_graph_mvcc_undirected_graph() {
    let mut graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::undirected(),
    );

    // Add an edge
    graph
        .add_edge(
            b"node1",
            b"connected",
            b"node2",
            b"edge1",
            TransactionId::from(1),
            LogSequenceNumber::from(1),
        )
        .unwrap();
    graph
        .commit_versions(TransactionId::from(1), LogSequenceNumber::from(10))
        .unwrap();

    // In undirected graph, edge should be visible from both directions
    let cursor = graph.outgoing(b"node1", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(edges.len(), 1);

    let cursor = graph.outgoing(b"node2", None).unwrap();
    let edges = cursor.collect_all().unwrap();
    assert_eq!(
        edges.len(),
        1,
        "Undirected edge should be visible from both nodes"
    );
}

#[test]
fn test_graph_mvcc_capabilities() {
    let graph = MemoryGraphTable::new(
        TableId::from(1),
        "test_graph".to_string(),
        GraphConfig::default(),
    );

    let caps = nanostore::table::Table::capabilities(&graph);
    assert!(caps.mvcc_native, "Graph table should support native MVCC");
    assert!(
        caps.memory_resident,
        "Graph table should be memory resident"
    );
}

// Made with Bob
