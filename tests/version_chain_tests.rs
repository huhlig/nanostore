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

use nanostore::snap::{Snapshot, SnapshotId};
use nanostore::txn::{TransactionId, VersionChain};
use nanostore::wal::LogSequenceNumber;

fn snapshot(lsn: u64, active_txns: Vec<u64>) -> Snapshot {
    Snapshot::new(
        SnapshotId::from(1),
        "test".to_string(),
        LogSequenceNumber::from(lsn),
        0,
        0,
        active_txns.into_iter().map(TransactionId::from).collect(),
    )
}

#[test]
fn find_visible_version_returns_newest_visible_committed_value() {
    let mut chain = VersionChain::new(b"v1".to_vec(), TransactionId::from(1));
    chain.commit(LogSequenceNumber::from(10));

    let mut chain = chain.prepend(b"v2".to_vec(), TransactionId::from(2));
    chain.commit(LogSequenceNumber::from(20));

    let mut chain = chain.prepend(b"v3".to_vec(), TransactionId::from(3));
    chain.commit(LogSequenceNumber::from(30));

    let snap = snapshot(25, vec![]);
    assert_eq!(chain.find_visible_inline(&snap), Some(&b"v2"[..]));
}

#[test]
fn find_visible_version_skips_uncommitted_head() {
    let mut chain = VersionChain::new(b"stable".to_vec(), TransactionId::from(1));
    chain.commit(LogSequenceNumber::from(10));

    let chain = chain.prepend(b"pending".to_vec(), TransactionId::from(2));

    let snap = snapshot(100, vec![]);
    assert_eq!(chain.find_visible_inline(&snap), Some(&b"stable"[..]));
}

#[test]
fn find_visible_version_skips_versions_from_active_snapshot_transactions() {
    let mut chain = VersionChain::new(b"base".to_vec(), TransactionId::from(1));
    chain.commit(LogSequenceNumber::from(10));

    let mut chain = chain.prepend(b"inflight-at-snapshot".to_vec(), TransactionId::from(5));
    chain.commit(LogSequenceNumber::from(20));

    let snap = snapshot(30, vec![5]);
    assert_eq!(chain.find_visible_inline(&snap), Some(&b"base"[..]));
}

#[test]
fn find_visible_version_returns_none_when_no_version_is_visible() {
    let chain = VersionChain::new(b"pending".to_vec(), TransactionId::from(1));
    let snap = snapshot(100, vec![]);
    assert_eq!(chain.find_visible_version(&snap), None);
}

#[test]
fn vacuum_removes_only_obsolete_committed_versions() {
    let mut chain = VersionChain::new(b"v1".to_vec(), TransactionId::from(1));
    chain.commit(LogSequenceNumber::from(10));

    let mut chain = chain.prepend(b"v2".to_vec(), TransactionId::from(2));
    chain.commit(LogSequenceNumber::from(20));

    let mut chain = chain.prepend(b"v3".to_vec(), TransactionId::from(3));
    chain.commit(LogSequenceNumber::from(30));

    let mut chain = chain.prepend(b"v4".to_vec(), TransactionId::from(4));
    chain.commit(LogSequenceNumber::from(40));

    let (removed, _freed_refs) = chain.vacuum(LogSequenceNumber::from(35));
    assert_eq!(removed, 2);

    let snap_new = snapshot(100, vec![]);
    assert_eq!(chain.find_visible_inline(&snap_new), Some(&b"v4"[..]));

    let snap_boundary = snapshot(35, vec![]);
    assert_eq!(chain.find_visible_inline(&snap_boundary), Some(&b"v3"[..]));
}

#[test]
fn vacuum_preserves_uncommitted_versions() {
    let mut chain = VersionChain::new(b"v1".to_vec(), TransactionId::from(1));
    chain.commit(LogSequenceNumber::from(10));

    let mut chain = chain.prepend(b"v2".to_vec(), TransactionId::from(2));
    chain.commit(LogSequenceNumber::from(20));

    let chain = chain.prepend(b"pending".to_vec(), TransactionId::from(3));
    let mut chain = chain;

    let (removed, _freed_refs) = chain.vacuum(LogSequenceNumber::from(25));
    assert_eq!(removed, 1);

    let snap = snapshot(100, vec![]);
    assert_eq!(chain.find_visible_inline(&snap), Some(&b"v2"[..]));
}

// Made with Bob
