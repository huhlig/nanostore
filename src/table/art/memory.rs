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

//! In-memory Adaptive Radix Tree (ART) table implementation.
//!
//! This module provides a memory-resident ART implementation optimized for:
//! - Fast prefix-based lookups and scans
//! - Adaptive node sizes (Node4, Node16, Node48, Node256)
//! - Path compression for space efficiency
//! - Autocomplete and prefix-based index use cases

use crate::snap::Snapshot;
use crate::table::{
    BatchOps, BatchReport, Flushable, MutableTable, Mutation, OrderedScan, PointLookup, PrefixScan,
    SearchableTable, Table, TableCapabilities, TableCursor, TableEngineKind, TableReader,
    TableResult, TableStatistics, TableWriter, WriteBatch,
};
use crate::txn::{TransactionId, VersionChain};
use crate::types::{Bound, KeyBuf, ScanBounds, TableId, ValueBuf};
use crate::wal::LogSequenceNumber;
use std::sync::{Arc, RwLock};

const NODE4_MAX: usize = 4;
const NODE16_MAX: usize = 16;
const NODE48_MAX: usize = 48;

fn default_children_48() -> [Option<Box<ARTNode>>; NODE48_MAX] {
    [const { None }; NODE48_MAX]
}

fn default_children_256() -> [Option<Box<ARTNode>>; 256] {
    [const { None }; 256]
}

#[derive(Debug)]
struct Leaf {
    key: Vec<u8>,
    chain: VersionChain,
}

impl Leaf {
    fn new(key: Vec<u8>, value: Vec<u8>, tx_id: TransactionId) -> Self {
        Self {
            key,
            chain: VersionChain::new(value, tx_id),
        }
    }
}

#[derive(Debug)]
enum ARTNode {
    Leaf(Leaf),
    Node4 {
        prefix: Vec<u8>,
        prefix_len: usize,
        keys: [u8; NODE4_MAX],
        children: [Option<Box<ARTNode>>; NODE4_MAX],
        count: usize,
    },
    Node16 {
        prefix: Vec<u8>,
        prefix_len: usize,
        keys: [u8; NODE16_MAX],
        children: [Option<Box<ARTNode>>; NODE16_MAX],
        count: usize,
    },
    Node48 {
        prefix: Vec<u8>,
        prefix_len: usize,
        index: [Option<u8>; 256],
        children: [Option<Box<ARTNode>>; NODE48_MAX],
        count: usize,
    },
    Node256 {
        prefix: Vec<u8>,
        prefix_len: usize,
        children: [Option<Box<ARTNode>>; 256],
        count: usize,
    },
}

impl ARTNode {
    fn new_node4(prefix: Vec<u8>, prefix_len: usize) -> Self {
        Self::Node4 {
            prefix,
            prefix_len,
            keys: [0; NODE4_MAX],
            children: [const { None }; NODE4_MAX],
            count: 0,
        }
    }

    fn new_node16(prefix: Vec<u8>, prefix_len: usize) -> Self {
        Self::Node16 {
            prefix,
            prefix_len,
            keys: [0; NODE16_MAX],
            children: [const { None }; NODE16_MAX],
            count: 0,
        }
    }

    fn new_node48(prefix: Vec<u8>, prefix_len: usize) -> Self {
        Self::Node48 {
            prefix,
            prefix_len,
            index: [None; 256],
            children: default_children_48(),
            count: 0,
        }
    }

    fn new_node256(prefix: Vec<u8>, prefix_len: usize) -> Self {
        Self::Node256 {
            prefix,
            prefix_len,
            children: default_children_256(),
            count: 0,
        }
    }

    fn prefix(&self) -> &[u8] {
        match self {
            Self::Leaf(leaf) => &leaf.key,
            Self::Node4 { prefix, .. }
            | Self::Node16 { prefix, .. }
            | Self::Node48 { prefix, .. }
            | Self::Node256 { prefix, .. } => prefix,
        }
    }

    fn prefix_len(&self) -> usize {
        match self {
            Self::Leaf(_) => 0,
            Self::Node4 { prefix_len, .. }
            | Self::Node16 { prefix_len, .. }
            | Self::Node48 { prefix_len, .. }
            | Self::Node256 { prefix_len, .. } => *prefix_len,
        }
    }

    fn count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Node4 { count, .. }
            | Self::Node16 { count, .. }
            | Self::Node48 { count, .. }
            | Self::Node256 { count, .. } => *count,
        }
    }

    fn find_child(&self, byte: u8) -> Option<&ARTNode> {
        match self {
            Self::Node4 {
                keys,
                children,
                count,
                ..
            } => (0..*count)
                .find(|i| keys[*i] == byte)
                .and_then(|i| children[i].as_deref()),
            Self::Node16 {
                keys,
                children,
                count,
                ..
            } => (0..*count)
                .find(|i| keys[*i] == byte)
                .and_then(|i| children[i].as_deref()),
            Self::Node48 {
                index, children, ..
            } => index[byte as usize].and_then(|idx| children[idx as usize].as_deref()),
            Self::Node256 { children, .. } => children[byte as usize].as_deref(),
            Self::Leaf(_) => None,
        }
    }

    fn find_child_mut(&mut self, byte: u8) -> Option<&mut Box<ARTNode>> {
        match self {
            Self::Node4 {
                keys,
                children,
                count,
                ..
            } => (0..*count)
                .find(|i| keys[*i] == byte)
                .and_then(|i| children[i].as_mut()),
            Self::Node16 {
                keys,
                children,
                count,
                ..
            } => (0..*count)
                .find(|i| keys[*i] == byte)
                .and_then(|i| children[i].as_mut()),
            Self::Node48 {
                index, children, ..
            } => index[byte as usize].and_then(|idx| children[idx as usize].as_mut()),
            Self::Node256 { children, .. } => children[byte as usize].as_mut(),
            Self::Leaf(_) => None,
        }
    }

    fn add_child(&mut self, byte: u8, child: Box<ARTNode>) -> bool {
        match self {
            Self::Node4 {
                keys,
                children,
                count,
                ..
            } => {
                if *count >= NODE4_MAX {
                    return false;
                }
                let idx = *count;
                keys[idx] = byte;
                children[idx] = Some(child);
                *count += 1;
                true
            }
            Self::Node16 {
                keys,
                children,
                count,
                ..
            } => {
                if *count >= NODE16_MAX {
                    return false;
                }
                let idx = *count;
                keys[idx] = byte;
                children[idx] = Some(child);
                *count += 1;
                true
            }
            Self::Node48 {
                index,
                children,
                count,
                ..
            } => {
                if *count >= NODE48_MAX {
                    return false;
                }
                let idx = (0..NODE48_MAX).find(|i| children[*i].is_none()).unwrap();
                index[byte as usize] = Some(idx as u8);
                children[idx] = Some(child);
                *count += 1;
                true
            }
            Self::Node256 {
                children, count, ..
            } => {
                if children[byte as usize].is_some() {
                    return false;
                }
                children[byte as usize] = Some(child);
                *count += 1;
                true
            }
            Self::Leaf(_) => false,
        }
    }

    fn remove_child(&mut self, byte: u8) -> Option<Box<ARTNode>> {
        match self {
            Self::Node4 {
                keys,
                children,
                count,
                ..
            } => (0..*count).find(|i| keys[*i] == byte).and_then(|i| {
                let result = children[i].take();
                for j in i..*count - 1 {
                    keys[j] = keys[j + 1];
                    children[j] = children[j + 1].take();
                }
                keys[*count - 1] = 0;
                *count -= 1;
                result
            }),
            Self::Node16 {
                keys,
                children,
                count,
                ..
            } => (0..*count).find(|i| keys[*i] == byte).and_then(|i| {
                let result = children[i].take();
                for j in i..*count - 1 {
                    keys[j] = keys[j + 1];
                    children[j] = children[j + 1].take();
                }
                keys[*count - 1] = 0;
                *count -= 1;
                result
            }),
            Self::Node48 {
                index,
                children,
                count,
                ..
            } => index[byte as usize].take().and_then(|idx| {
                *count -= 1;
                children[idx as usize].take()
            }),
            Self::Node256 {
                children, count, ..
            } => {
                if children[byte as usize].is_some() {
                    *count -= 1;
                    children[byte as usize].take()
                } else {
                    None
                }
            }
            Self::Leaf(_) => None,
        }
    }

    fn grow(&mut self) {
        let (prefix, prefix_len, count) = match self {
            Self::Node4 {
                prefix,
                prefix_len,
                count,
                ..
            } => (prefix.clone(), *prefix_len, *count),
            Self::Node16 {
                prefix,
                prefix_len,
                count,
                ..
            } => (prefix.clone(), *prefix_len, *count),
            Self::Node48 {
                prefix,
                prefix_len,
                count,
                ..
            } => (prefix.clone(), *prefix_len, *count),
            _ => return,
        };

        let mut new_node = match self {
            Self::Node4 { .. } => Self::new_node16(prefix, prefix_len),
            Self::Node16 { .. } => Self::new_node48(prefix, prefix_len),
            Self::Node48 { .. } => Self::new_node256(prefix, prefix_len),
            _ => return,
        };

        let bytes: Vec<u8> = match self {
            Self::Node4 { keys, count, .. } => (0..*count).map(|i| keys[i]).collect(),
            Self::Node16 { keys, count, .. } => (0..*count).map(|i| keys[i]).collect(),
            Self::Node48 { index, .. } => (0..=255u8)
                .filter(|b| index[*b as usize].is_some())
                .collect(),
            _ => return,
        };

        for byte in bytes {
            let child = match self {
                Self::Node4 {
                    keys,
                    children,
                    count,
                    ..
                } => (0..*count)
                    .find(|i| keys[*i] == byte)
                    .and_then(|i| children[i].take()),
                Self::Node16 {
                    keys,
                    children,
                    count,
                    ..
                } => (0..*count)
                    .find(|i| keys[*i] == byte)
                    .and_then(|i| children[i].take()),
                Self::Node48 {
                    index, children, ..
                } => index[byte as usize].and_then(|idx| children[idx as usize].take()),
                _ => None,
            };
            if let Some(child) = child {
                new_node.add_child(byte, child);
            }
        }

        if let Self::Node16 { count: c, .. }
        | Self::Node48 { count: c, .. }
        | Self::Node256 { count: c, .. } = &mut new_node
        {
            *c = count;
        }
        *self = new_node;
    }

    fn shrink(&mut self) {
        let (prefix, prefix_len, count) = match self {
            Self::Node256 {
                prefix,
                prefix_len,
                count,
                ..
            } => (prefix.clone(), *prefix_len, *count),
            Self::Node48 {
                prefix,
                prefix_len,
                count,
                ..
            } => (prefix.clone(), *prefix_len, *count),
            Self::Node16 {
                prefix,
                prefix_len,
                count,
                ..
            } => (prefix.clone(), *prefix_len, *count),
            _ => return,
        };

        let mut new_node = match self {
            Self::Node256 { .. } => Self::new_node48(prefix, prefix_len),
            Self::Node48 { .. } => Self::new_node16(prefix, prefix_len),
            Self::Node16 { .. } => Self::new_node4(prefix, prefix_len),
            _ => return,
        };

        let bytes: Vec<u8> = match self {
            Self::Node256 { children, .. } => (0..=255u8)
                .filter(|b| children[*b as usize].is_some())
                .collect(),
            Self::Node48 { index, .. } => (0..=255u8)
                .filter(|b| index[*b as usize].is_some())
                .collect(),
            Self::Node16 { keys, count, .. } => (0..*count).map(|i| keys[i]).collect(),
            _ => return,
        };

        for byte in bytes {
            let child = match self {
                Self::Node256 { children, .. } => children[byte as usize].take(),
                Self::Node48 {
                    index, children, ..
                } => index[byte as usize].and_then(|idx| children[idx as usize].take()),
                Self::Node16 {
                    keys,
                    children,
                    count,
                    ..
                } => (0..*count)
                    .find(|i| keys[*i] == byte)
                    .and_then(|i| children[i].take()),
                _ => None,
            };
            if let Some(child) = child {
                new_node.add_child(byte, child);
            }
        }

        if let Self::Node48 { count: c, .. }
        | Self::Node16 { count: c, .. }
        | Self::Node4 { count: c, .. } = &mut new_node
        {
            *c = count;
        }
        *self = new_node;
    }

    fn children_iter(&self) -> Box<dyn Iterator<Item = (u8, &ARTNode)> + '_> {
        match self {
            Self::Node4 {
                keys,
                children,
                count,
                ..
            } => Box::new(
                (0..*count).filter_map(move |i| children[i].as_deref().map(|c| (keys[i], c))),
            ),
            Self::Node16 {
                keys,
                children,
                count,
                ..
            } => Box::new(
                (0..*count).filter_map(move |i| children[i].as_deref().map(|c| (keys[i], c))),
            ),
            Self::Node48 {
                index, children, ..
            } => Box::new((0..=255u8).filter_map(move |byte| {
                index[byte as usize]
                    .and_then(|idx| children[idx as usize].as_deref().map(|c| (byte, c)))
            })),
            Self::Node256 { children, .. } => Box::new(
                (0..=255u8)
                    .filter_map(move |byte| children[byte as usize].as_deref().map(|c| (byte, c))),
            ),
            Self::Leaf(_) => Box::new(std::iter::empty()),
        }
    }
}

pub struct MemoryART {
    id: TableId,
    name: String,
    root: Arc<RwLock<Option<Box<ARTNode>>>>,
    memory_usage: Arc<RwLock<usize>>,
    memory_budget: usize,
}

impl MemoryART {
    pub fn new(id: TableId, name: String) -> Self {
        Self::with_budget(id, name, 64 * 1024 * 1024)
    }

    pub fn with_budget(id: TableId, name: String, memory_budget: usize) -> Self {
        Self {
            id,
            name,
            root: Arc::new(RwLock::new(None)),
            memory_usage: Arc::new(RwLock::new(0)),
            memory_budget,
        }
    }

    fn get_memory_usage(&self) -> usize {
        *self.memory_usage.read().unwrap()
    }

    fn update_memory_usage(&self, delta: isize) {
        let mut usage = self.memory_usage.write().unwrap();
        if delta < 0 {
            *usage = usage.saturating_sub(delta.unsigned_abs());
        } else {
            *usage = usage.saturating_add(delta as usize);
        }
    }

    fn insert(&self, key: &[u8], value: Vec<u8>, tx_id: TransactionId) {
        let mut root = self.root.write().unwrap();
        let old_size = Self::estimate_tree_size(root.as_deref());
        Self::insert_recursive(&mut root, key, value, tx_id, 0);
        let new_size = Self::estimate_tree_size(root.as_deref());
        self.update_memory_usage(new_size as isize - old_size as isize);
    }

    fn insert_recursive(
        node: &mut Option<Box<ARTNode>>,
        key: &[u8],
        value: Vec<u8>,
        tx_id: TransactionId,
        depth: usize,
    ) {
        if node.is_none() {
            *node = Some(Box::new(ARTNode::Leaf(Leaf::new(
                key.to_vec(),
                value,
                tx_id,
            ))));
            return;
        }

        let current = node.as_mut().unwrap();
        match current.as_mut() {
            ARTNode::Leaf(leaf) => {
                if leaf.key == key {
                    let old_chain =
                        std::mem::replace(&mut leaf.chain, VersionChain::new(vec![], tx_id));
                    leaf.chain = old_chain.prepend(value, tx_id);
                } else {
                    let existing_key_suffix = &leaf.key[depth..];
                    let new_key_suffix = &key[depth..];
                    let common_prefix = Self::common_prefix(existing_key_suffix, new_key_suffix);

                    let mut new_node = ARTNode::new_node4(
                        key[depth..depth + common_prefix].to_vec(),
                        common_prefix,
                    );

                    let old_leaf = Box::new(ARTNode::Leaf(Leaf::new(
                        leaf.key.clone(),
                        Vec::from(leaf.chain.value.as_inline().unwrap_or(&[])),
                        tx_id,
                    )));
                    let new_leaf = Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value, tx_id)));

                    let old_byte = if depth + common_prefix < leaf.key.len() {
                        leaf.key[depth + common_prefix]
                    } else {
                        0
                    };
                    let new_byte = if depth + common_prefix < key.len() {
                        key[depth + common_prefix]
                    } else {
                        0
                    };

                    new_node.add_child(old_byte, old_leaf);
                    new_node.add_child(new_byte, new_leaf);
                    *node = Some(Box::new(new_node));
                }
            }
            _ => {
                let prefix_len = current.prefix_len();
                let prefix = current.prefix().to_vec();
                let key_suffix = &key[depth..];

                if key_suffix.len() < prefix_len || key_suffix[..prefix_len] != prefix[..prefix_len]
                {
                    let common_prefix = Self::common_prefix(&prefix, key_suffix);
                    let mut new_node =
                        ARTNode::new_node4(key_suffix[..common_prefix].to_vec(), common_prefix);

                    let old_byte = if common_prefix < prefix_len {
                        prefix[common_prefix]
                    } else {
                        0
                    };
                    let cloned = Self::clone_node(current);
                    new_node.add_child(old_byte, cloned);

                    let new_byte = if depth + common_prefix < key.len() {
                        key[depth + common_prefix]
                    } else {
                        0
                    };
                    new_node.add_child(
                        new_byte,
                        Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value, tx_id))),
                    );
                    *node = Some(Box::new(new_node));
                    return;
                }

                let new_depth = depth + prefix_len;
                let byte = if new_depth == key.len() {
                    0
                } else {
                    key[new_depth]
                };

                if let Some(child) = current.find_child_mut(byte) {
                    Self::insert_into_boxed(child, key, value.clone(), tx_id, new_depth);
                } else {
                    let new_leaf =
                        Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value.clone(), tx_id)));
                    if !current.add_child(byte, new_leaf) {
                        current.grow();
                        current.add_child(
                            byte,
                            Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value, tx_id))),
                        );
                    }
                }
            }
        }
    }

    fn insert_into_boxed(
        boxed: &mut Box<ARTNode>,
        key: &[u8],
        value: Vec<u8>,
        tx_id: TransactionId,
        depth: usize,
    ) {
        match boxed.as_mut() {
            ARTNode::Leaf(leaf) => {
                if leaf.key == key {
                    let old_chain =
                        std::mem::replace(&mut leaf.chain, VersionChain::new(vec![], tx_id));
                    leaf.chain = old_chain.prepend(value, tx_id);
                } else {
                    let existing_key_suffix = &leaf.key[depth..];
                    let new_key_suffix = &key[depth..];
                    let common_prefix = Self::common_prefix(existing_key_suffix, new_key_suffix);

                    let mut new_node = ARTNode::new_node4(
                        key[depth..depth + common_prefix].to_vec(),
                        common_prefix,
                    );

                    let old_leaf = Box::new(ARTNode::Leaf(Leaf::new(
                        leaf.key.clone(),
                        Vec::from(leaf.chain.value.as_inline().unwrap_or(&[])),
                        tx_id,
                    )));
                    let new_leaf = Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value, tx_id)));

                    let old_byte = if depth + common_prefix < leaf.key.len() {
                        leaf.key[depth + common_prefix]
                    } else {
                        0
                    };
                    let new_byte = if depth + common_prefix < key.len() {
                        key[depth + common_prefix]
                    } else {
                        0
                    };

                    new_node.add_child(old_byte, old_leaf);
                    new_node.add_child(new_byte, new_leaf);
                    *boxed = Box::new(new_node);
                }
            }
            internal => {
                let prefix_len = internal.prefix_len();
                let prefix = internal.prefix().to_vec();
                let key_suffix = &key[depth..];

                if key_suffix.len() < prefix_len || key_suffix[..prefix_len] != prefix[..prefix_len]
                {
                    let common_prefix = Self::common_prefix(&prefix, key_suffix);
                    let mut new_node =
                        ARTNode::new_node4(key_suffix[..common_prefix].to_vec(), common_prefix);

                    let old_byte = if common_prefix < prefix_len {
                        prefix[common_prefix]
                    } else {
                        0
                    };
                    let cloned = Self::clone_node(internal);
                    new_node.add_child(old_byte, cloned);

                    let new_byte = if depth + common_prefix < key.len() {
                        key[depth + common_prefix]
                    } else {
                        0
                    };
                    new_node.add_child(
                        new_byte,
                        Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value, tx_id))),
                    );
                    *boxed = Box::new(new_node);
                    return;
                }

                let new_depth = depth + prefix_len;
                let byte = if new_depth == key.len() {
                    0
                } else {
                    key[new_depth]
                };

                if let Some(child) = internal.find_child_mut(byte) {
                    Self::insert_into_boxed(child, key, value.clone(), tx_id, new_depth);
                } else {
                    let new_leaf =
                        Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value.clone(), tx_id)));
                    if !internal.add_child(byte, new_leaf) {
                        internal.grow();
                        internal.add_child(
                            byte,
                            Box::new(ARTNode::Leaf(Leaf::new(key.to_vec(), value, tx_id))),
                        );
                    }
                }
            }
        }
    }

    fn lookup(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> Option<Vec<u8>> {
        let root = self.root.read().unwrap();
        Self::lookup_recursive(root.as_deref(), key, snapshot_lsn, 0)
    }

    fn lookup_recursive(
        node: Option<&ARTNode>,
        key: &[u8],
        snapshot_lsn: LogSequenceNumber,
        depth: usize,
    ) -> Option<Vec<u8>> {
        let node = node?;
        match node {
            ARTNode::Leaf(leaf) => {
                if leaf.key == key {
                    let snapshot = Snapshot::new(
                        crate::snap::SnapshotId::from(0),
                        String::new(),
                        snapshot_lsn,
                        0,
                        0,
                        Vec::new(),
                    );
                    leaf.chain
                        .find_visible_inline(&snapshot)
                        .map(|v| Vec::from(v))
                } else {
                    None
                }
            }
            _ => {
                let prefix_len = node.prefix_len();
                let prefix = node.prefix();
                // Handle both absolute and relative prefixes
                // If prefix_len <= depth, the prefix is relative to current depth
                // If prefix_len > depth, the prefix is absolute (from start of key)
                let (start, end) = if prefix_len <= depth {
                    (depth, depth + prefix_len)
                } else {
                    (0, prefix_len)
                };
                if end > key.len() || key[start..end] != prefix[..prefix_len.min(end - start)] {
                    return None;
                }
                let new_depth = if prefix_len <= depth {
                    depth + prefix_len
                } else {
                    prefix_len
                };
                if new_depth >= key.len() {
                    if let Some(child) = node.find_child(0) {
                        return Self::lookup_recursive(Some(child), key, snapshot_lsn, new_depth);
                    }
                    return None;
                }
                let byte = key[new_depth];
                if let Some(child) = node.find_child(byte) {
                    Self::lookup_recursive(Some(child), key, snapshot_lsn, new_depth)
                } else {
                    None
                }
            }
        }
    }

    fn delete(&self, key: &[u8]) -> bool {
        let mut root = self.root.write().unwrap();
        let old_size = Self::estimate_tree_size(root.as_deref());
        let result = if let Some(boxed) = root.as_mut() {
            Self::delete_from_boxed(boxed, key, 0)
        } else {
            false
        };
        if result {
            if let Some(boxed) = root.as_ref() {
                if boxed.count() == 0 {
                    *root = None;
                }
            }
            let new_size = Self::estimate_tree_size(root.as_deref());
            self.update_memory_usage(new_size as isize - old_size as isize);
        }
        result
    }

    fn delete_from_boxed(boxed: &mut Box<ARTNode>, key: &[u8], depth: usize) -> bool {
        match boxed.as_mut() {
            ARTNode::Leaf(leaf) => {
                if leaf.key == key {
                    return true;
                }
                false
            }
            _ => {
                let prefix_len = boxed.prefix_len();
                let prefix = boxed.prefix().to_vec();
                let key_suffix = &key[depth..];
                if key_suffix.len() < prefix_len || key_suffix[..prefix_len] != prefix[..prefix_len]
                {
                    return false;
                }
                let new_depth = depth + prefix_len;
                let byte = if new_depth >= key.len() {
                    0
                } else {
                    key[new_depth]
                };

                if let Some(mut child) = boxed.remove_child(byte) {
                    if Self::delete_from_boxed(&mut child, key, new_depth) {
                        return true;
                    }
                    boxed.add_child(byte, child);
                    if boxed.count() <= 1 {
                        boxed.shrink();
                    }
                    return true;
                }
                false
            }
        }
    }

    fn delete_recursive(_node: &mut Option<Box<ARTNode>>, _key: &[u8], _depth: usize) -> bool {
        unreachable!("use delete_from_boxed instead")
    }

    fn common_prefix(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
    }

    fn clone_node(node: &ARTNode) -> Box<ARTNode> {
        match node {
            ARTNode::Leaf(leaf) => Box::new(ARTNode::Leaf(Leaf {
                key: leaf.key.clone(),
                chain: leaf.chain.clone(),
            })),
            ARTNode::Node4 {
                prefix,
                prefix_len,
                keys,
                children,
                count,
            } => {
                let mut new_node = ARTNode::new_node4(prefix.clone(), *prefix_len);
                for i in 0..*count {
                    if let Some(child) = &children[i] {
                        new_node.add_child(keys[i], Self::clone_node(child));
                    }
                }
                if let ARTNode::Node4 { count: c, .. } = &mut new_node {
                    *c = *count;
                }
                Box::new(new_node)
            }
            ARTNode::Node16 {
                prefix,
                prefix_len,
                keys,
                children,
                count,
            } => {
                let mut new_node = ARTNode::new_node16(prefix.clone(), *prefix_len);
                for i in 0..*count {
                    if let Some(child) = &children[i] {
                        new_node.add_child(keys[i], Self::clone_node(child));
                    }
                }
                if let ARTNode::Node16 { count: c, .. } = &mut new_node {
                    *c = *count;
                }
                Box::new(new_node)
            }
            ARTNode::Node48 {
                prefix,
                prefix_len,
                index,
                children,
                count,
            } => {
                let mut new_node = ARTNode::new_node48(prefix.clone(), *prefix_len);
                for byte in 0..=255u8 {
                    if let Some(idx) = index[byte as usize] {
                        if let Some(child) = &children[idx as usize] {
                            new_node.add_child(byte, Self::clone_node(child));
                        }
                    }
                }
                if let ARTNode::Node48 { count: c, .. } = &mut new_node {
                    *c = *count;
                }
                Box::new(new_node)
            }
            ARTNode::Node256 {
                prefix,
                prefix_len,
                children,
                count,
            } => {
                let mut new_node = ARTNode::new_node256(prefix.clone(), *prefix_len);
                for byte in 0..=255u8 {
                    if let Some(child) = &children[byte as usize] {
                        new_node.add_child(byte, Self::clone_node(child));
                    }
                }
                if let ARTNode::Node256 { count: c, .. } = &mut new_node {
                    *c = *count;
                }
                Box::new(new_node)
            }
        }
    }

    fn estimate_tree_size(node: Option<&ARTNode>) -> usize {
        match node {
            None => 0,
            Some(ARTNode::Leaf(leaf)) => {
                leaf.key.len() + leaf.chain.value.len() + std::mem::size_of::<Leaf>()
            }
            Some(ARTNode::Node4 {
                prefix,
                children,
                count,
                ..
            }) => {
                prefix.len()
                    + std::mem::size_of::<ARTNode>()
                    + (0..*count)
                        .map(|i| Self::estimate_tree_size(children[i].as_deref()))
                        .sum::<usize>()
            }
            Some(ARTNode::Node16 {
                prefix,
                children,
                count,
                ..
            }) => {
                prefix.len()
                    + std::mem::size_of::<ARTNode>()
                    + (0..*count)
                        .map(|i| Self::estimate_tree_size(children[i].as_deref()))
                        .sum::<usize>()
            }
            Some(ARTNode::Node48 {
                prefix,
                index,
                children,
                ..
            }) => {
                prefix.len()
                    + std::mem::size_of::<ARTNode>()
                    + (0..=255u8)
                        .filter_map(|b| index[b as usize])
                        .map(|idx| Self::estimate_tree_size(children[idx as usize].as_deref()))
                        .sum::<usize>()
            }
            Some(ARTNode::Node256 {
                prefix, children, ..
            }) => {
                prefix.len()
                    + std::mem::size_of::<ARTNode>()
                    + children
                        .iter()
                        .filter_map(|c| c.as_deref())
                        .map(|n| Self::estimate_tree_size(Some(n)))
                        .sum::<usize>()
            }
        }
    }

    fn collect_keys_in_bounds(
        node: Option<&ARTNode>,
        bounds: &ScanBounds,
        snapshot_lsn: LogSequenceNumber,
        result: &mut Vec<(Vec<u8>, Vec<u8>)>,
    ) {
        let node = match node {
            Some(n) => n,
            None => return,
        };

        match node {
            ARTNode::Leaf(leaf) => {
                if Self::is_in_bounds(&leaf.key, bounds) {
                    let snapshot = Snapshot::new(
                        crate::snap::SnapshotId::from(0),
                        String::new(),
                        snapshot_lsn,
                        0,
                        0,
                        Vec::new(),
                    );
                    if let Some(value) = leaf.chain.find_visible_inline(&snapshot) {
                        eprintln!("Collecting leaf: {:?}", String::from_utf8_lossy(&leaf.key));
                        result.push((leaf.key.clone(), Vec::from(value)));
                    } else {
                        eprintln!("Leaf not visible: {:?}", String::from_utf8_lossy(&leaf.key));
                    }
                }
            }
            _ => {
                eprintln!(
                    "Internal node: prefix={:?}, count={}",
                    String::from_utf8_lossy(node.prefix()),
                    node.count()
                );
                for (byte, child) in node.children_iter() {
                    eprintln!("  Child byte={}", byte);
                    Self::collect_keys_in_bounds(Some(child), bounds, snapshot_lsn, result);
                }
            }
        }
    }

    fn is_in_bounds(key: &[u8], bounds: &ScanBounds) -> bool {
        match bounds {
            ScanBounds::All => true,
            ScanBounds::Prefix(prefix) => key.starts_with(&prefix.0),
            ScanBounds::Range { start, end } => {
                let after_start = match start {
                    Bound::Included(k) => key >= k.0.as_slice(),
                    Bound::Excluded(k) => key > k.0.as_slice(),
                    Bound::Unbounded => true,
                };
                let before_end = match end {
                    Bound::Included(k) => key <= k.0.as_slice(),
                    Bound::Excluded(k) => key < k.0.as_slice(),
                    Bound::Unbounded => true,
                };
                after_start && before_end
            }
        }
    }

    fn count_leaves(node: Option<&ARTNode>) -> usize {
        match node {
            None => 0,
            Some(ARTNode::Leaf(_)) => 1,
            Some(ARTNode::Node4 {
                children, count, ..
            }) => (0..*count)
                .map(|i| Self::count_leaves(children[i].as_deref()))
                .sum(),
            Some(ARTNode::Node16 {
                children, count, ..
            }) => (0..*count)
                .map(|i| Self::count_leaves(children[i].as_deref()))
                .sum(),
            Some(ARTNode::Node48 {
                index, children, ..
            }) => (0..=255u8)
                .filter_map(|b| index[b as usize])
                .map(|idx| Self::count_leaves(children[idx as usize].as_deref()))
                .sum(),
            Some(ARTNode::Node256 { children, .. }) => children
                .iter()
                .filter_map(|c| c.as_deref())
                .map(|n| Self::count_leaves(Some(n)))
                .sum(),
        }
    }

    fn commit_all(node: &mut Option<Box<ARTNode>>, lsn: LogSequenceNumber, tx_id: TransactionId) {
        if let Some(n) = node {
            Self::commit_from_boxed(n, lsn, tx_id);
        }
    }

    fn commit_from_boxed(boxed: &mut Box<ARTNode>, lsn: LogSequenceNumber, tx_id: TransactionId) {
        match boxed.as_mut() {
            ARTNode::Leaf(leaf) => {
                if leaf.chain.created_by == tx_id && leaf.chain.commit_lsn.is_none() {
                    leaf.chain.commit(lsn);
                }
            }
            ARTNode::Node4 {
                children, count, ..
            } => {
                for i in 0..*count {
                    if let Some(child) = &mut children[i] {
                        Self::commit_from_boxed(child, lsn, tx_id);
                    }
                }
            }
            ARTNode::Node16 {
                children, count, ..
            } => {
                for i in 0..*count {
                    if let Some(child) = &mut children[i] {
                        Self::commit_from_boxed(child, lsn, tx_id);
                    }
                }
            }
            ARTNode::Node48 {
                index, children, ..
            } => {
                for byte in 0..=255u8 {
                    if let Some(idx) = index[byte as usize] {
                        if let Some(child) = &mut children[idx as usize] {
                            Self::commit_from_boxed(child, lsn, tx_id);
                        }
                    }
                }
            }
            ARTNode::Node256 { children, .. } => {
                for child in children.iter_mut().flatten() {
                    Self::commit_from_boxed(child, lsn, tx_id);
                }
            }
        }
    }

    /// Vacuum obsolete versions from all entries in the tree.
    ///
    /// Recursively traverses the ART and calls VersionChain::vacuum() on each
    /// leaf entry, removing versions older than min_visible_lsn while preserving
    /// one old version as a base.
    ///
    /// Returns the total count of removed versions.
    pub fn vacuum(&self, min_visible_lsn: LogSequenceNumber) -> TableResult<usize> {
        let mut root = self.root.write().unwrap();
        Ok(Self::vacuum_node(root.as_mut(), min_visible_lsn))
    }

    /// Vacuum a single node recursively.
    fn vacuum_node(node: Option<&mut Box<ARTNode>>, min_visible_lsn: LogSequenceNumber) -> usize {
        let Some(node) = node else {
            return 0;
        };

        match node.as_mut() {
            ARTNode::Leaf(leaf) => {
                // Vacuum the leaf's version chain
                let (removed, _freed_refs) = leaf.chain.vacuum(min_visible_lsn);
                // Note: freed_refs contains ValueRefs for external values that need cleanup
                // For in-memory ART, we don't use external values, so we can ignore this
                removed
            }
            ARTNode::Node4 { children, .. } => {
                let mut total_removed = 0;
                for child in children.iter_mut() {
                    total_removed += Self::vacuum_node(child.as_mut(), min_visible_lsn);
                }
                total_removed
            }
            ARTNode::Node16 { children, .. } => {
                let mut total_removed = 0;
                for child in children.iter_mut() {
                    total_removed += Self::vacuum_node(child.as_mut(), min_visible_lsn);
                }
                total_removed
            }
            ARTNode::Node48 { children, .. } => {
                let mut total_removed = 0;
                for child in children.iter_mut() {
                    total_removed += Self::vacuum_node(child.as_mut(), min_visible_lsn);
                }
                total_removed
            }
            ARTNode::Node256 { children, .. } => {
                let mut total_removed = 0;
                for child in children.iter_mut() {
                    total_removed += Self::vacuum_node(child.as_mut(), min_visible_lsn);
                }
                total_removed
            }
        }
    }
}

impl Table for MemoryART {
    fn table_id(&self) -> TableId {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> TableEngineKind {
        TableEngineKind::Art
    }

    fn capabilities(&self) -> TableCapabilities {
        TableCapabilities {
            ordered: true,
            point_lookup: true,
            prefix_scan: true,
            reverse_scan: true,
            range_delete: true,
            merge_operator: false,
            mvcc_native: true,
            append_optimized: false,
            memory_resident: true,
            disk_resident: false,
            supports_compression: false,
            supports_encryption: false,
        }
    }

    fn stats(&self) -> TableResult<TableStatistics> {
        let root = self.root.read().unwrap();
        let count = Self::count_leaves(root.as_deref());
        Ok(TableStatistics {
            row_count: Some(count as u64),
            page_count: None, // Memory-based implementation doesn't use pages
            total_size_bytes: Some(self.get_memory_usage() as u64),
            key_stats: None,
            value_stats: None,
            histogram: None,
            last_updated_lsn: None,
        })
    }
}

impl SearchableTable for MemoryART {
    type Reader<'a> = MemoryARTReader<'a>;
    type Writer<'a> = MemoryARTWriter<'a>;

    fn reader(&self, snapshot_lsn: LogSequenceNumber) -> TableResult<Self::Reader<'_>> {
        Ok(MemoryARTReader {
            table: self,
            snapshot_lsn,
        })
    }

    fn writer(
        &self,
        tx_id: TransactionId,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Writer<'_>> {
        Ok(MemoryARTWriter {
            table: self,
            tx_id,
            snapshot_lsn,
            pending_changes: Vec::new(),
        })
    }
}

pub struct MemoryARTReader<'a> {
    table: &'a MemoryART,
    snapshot_lsn: LogSequenceNumber,
}

impl<'a> PointLookup for MemoryARTReader<'a> {
    fn get(&self, key: &[u8], snapshot_lsn: LogSequenceNumber) -> TableResult<Option<ValueBuf>> {
        Ok(self.table.lookup(key, snapshot_lsn).map(ValueBuf))
    }
}

impl<'a> OrderedScan for MemoryARTReader<'a> {
    type Cursor<'b>
        = MemoryARTCursor
    where
        Self: 'b;

    fn scan(
        &self,
        bounds: ScanBounds,
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>> {
        let root = self.table.root.read().unwrap();
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(root.as_deref(), &bounds, snapshot_lsn, &mut entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(MemoryARTCursor {
            entries,
            position: 0,
            snapshot_lsn,
        })
    }
}

impl<'a> TableReader for MemoryARTReader<'a> {
    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
    fn approximate_len(&self) -> TableResult<Option<u64>> {
        let root = self.table.root.read().unwrap();
        Ok(Some(MemoryART::count_leaves(root.as_deref()) as u64))
    }
}

impl<'a> PrefixScan for MemoryARTReader<'a> {
    fn scan_prefix(
        &self,
        prefix: &[u8],
        snapshot_lsn: LogSequenceNumber,
    ) -> TableResult<Self::Cursor<'_>> {
        self.scan(ScanBounds::Prefix(KeyBuf(prefix.to_vec())), snapshot_lsn)
    }
}

pub struct MemoryARTWriter<'a> {
    table: &'a MemoryART,
    tx_id: TransactionId,
    snapshot_lsn: LogSequenceNumber,
    pending_changes: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl<'a> MutableTable for MemoryARTWriter<'a> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> TableResult<u64> {
        self.pending_changes
            .push((key.to_vec(), Some(value.to_vec())));
        Ok((key.len() + value.len() + 16) as u64)
    }

    fn delete(&mut self, key: &[u8]) -> TableResult<bool> {
        let root = self.table.root.read().unwrap();
        let exists =
            MemoryART::lookup_recursive(root.as_deref(), key, LogSequenceNumber::from(u64::MAX), 0)
                .is_some();
        drop(root);
        if exists {
            self.pending_changes.push((key.to_vec(), None));
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn range_delete(&mut self, bounds: ScanBounds) -> TableResult<u64> {
        let root = self.table.root.read().unwrap();
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(
            root.as_deref(),
            &bounds,
            LogSequenceNumber::from(u64::MAX),
            &mut entries,
        );
        drop(root);
        let count = entries.len() as u64;
        for (key, _) in entries {
            self.pending_changes.push((key, None));
        }
        Ok(count)
    }
}

impl<'a> BatchOps for MemoryARTWriter<'a> {
    fn batch_get(&self, keys: &[&[u8]]) -> TableResult<Vec<Option<ValueBuf>>> {
        Ok(keys
            .iter()
            .map(|k| self.table.lookup(k, self.snapshot_lsn).map(ValueBuf))
            .collect())
    }

    fn apply_batch(&mut self, batch: WriteBatch<'_>) -> TableResult<BatchReport> {
        let mut report = BatchReport {
            attempted: batch.mutations.len() as u64,
            ..Default::default()
        };
        for mutation in batch.mutations {
            match mutation {
                Mutation::Put { key, value } => {
                    self.put(&key, &value)?;
                    report.applied += 1;
                    report.bytes_written += key.len() as u64 + value.len() as u64;
                }
                Mutation::Delete { key } => {
                    if self.delete(&key)? {
                        report.deleted += 1;
                    }
                    report.applied += 1;
                }
                Mutation::RangeDelete { bounds } => {
                    report.deleted += self.range_delete(bounds)?;
                    report.applied += 1;
                }
                Mutation::Merge { .. } => continue,
            }
        }
        Ok(report)
    }
}

impl<'a> Flushable for MemoryARTWriter<'a> {
    fn flush(&mut self) -> TableResult<()> {
        for (key, value_opt) in self.pending_changes.drain(..) {
            match value_opt {
                Some(value) => self.table.insert(&key, value, self.tx_id),
                None => {
                    self.table.delete(&key);
                }
            }
        }
        Ok(())
    }
}

impl<'a> MemoryARTWriter<'a> {
    /// Mark all versions created by this transaction as committed.
    ///
    /// This must be called after flush() to make the changes visible to readers.
    /// The commit_lsn is obtained from the WAL after writing the COMMIT record.
    pub fn commit_versions(&self, commit_lsn: LogSequenceNumber) -> TableResult<()> {
        let mut root = self.table.root.write().unwrap();
        MemoryART::commit_all(&mut root, commit_lsn, self.tx_id);
        Ok(())
    }
}

impl<'a> TableWriter for MemoryARTWriter<'a> {
    fn tx_id(&self) -> TransactionId {
        self.tx_id
    }
    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

pub struct MemoryARTCursor {
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    position: usize,
    snapshot_lsn: LogSequenceNumber,
}

impl TableCursor for MemoryARTCursor {
    fn valid(&self) -> bool {
        self.position < self.entries.len()
    }
    fn key(&self) -> Option<&[u8]> {
        if self.valid() {
            Some(&self.entries[self.position].0)
        } else {
            None
        }
    }
    fn value(&self) -> Option<&[u8]> {
        if self.valid() {
            Some(&self.entries[self.position].1)
        } else {
            None
        }
    }
    fn next(&mut self) -> TableResult<()> {
        if self.valid() {
            self.position += 1;
        }
        Ok(())
    }
    fn prev(&mut self) -> TableResult<()> {
        if self.position > 0 {
            self.position -= 1;
        }
        Ok(())
    }

    fn seek(&mut self, key: &[u8]) -> TableResult<()> {
        match self.entries.binary_search_by(|e| e.0.as_slice().cmp(key)) {
            Ok(idx) => self.position = idx,
            Err(idx) => self.position = idx.min(self.entries.len()),
        }
        Ok(())
    }

    fn seek_for_prev(&mut self, key: &[u8]) -> TableResult<()> {
        match self.entries.binary_search_by(|e| e.0.as_slice().cmp(key)) {
            Ok(idx) => self.position = idx,
            Err(idx) => self.position = idx.saturating_sub(1),
        }
        Ok(())
    }

    fn first(&mut self) -> TableResult<()> {
        self.position = 0;
        Ok(())
    }
    fn last(&mut self) -> TableResult<()> {
        if !self.entries.is_empty() {
            self.position = self.entries.len() - 1;
        }
        Ok(())
    }
    fn snapshot_lsn(&self) -> LogSequenceNumber {
        self.snapshot_lsn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_table(table: &MemoryART, lsn: LogSequenceNumber, tx_id: TransactionId) {
        let mut root = table.root.write().unwrap();
        MemoryART::commit_all(&mut root, lsn, tx_id);
    }

    #[test]
    fn test_art_direct_insert_lookup() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"key1", b"value1".to_vec(), tx_id);
        table.insert(b"key2", b"value2".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let lsn = LogSequenceNumber::from(20);
        assert_eq!(table.lookup(b"key1", lsn), Some(b"value1".to_vec()));
        assert_eq!(table.lookup(b"key2", lsn), Some(b"value2".to_vec()));
        assert_eq!(table.lookup(b"key3", lsn), None);
    }

    #[test]
    fn test_art_writer_flush_lookup() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"key1", b"value1").unwrap();
        writer.put(b"key2", b"value2").unwrap();
        writer.flush().unwrap();

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let lsn = LogSequenceNumber::from(20);
        assert_eq!(table.lookup(b"key1", lsn), Some(b"value1".to_vec()));
        assert_eq!(table.lookup(b"key2", lsn), Some(b"value2".to_vec()));
    }

    #[test]
    fn test_art_collect_keys_prefix() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"user:1", b"Alice".to_vec(), tx_id);
        table.insert(b"user:2", b"Bob".to_vec(), tx_id);
        table.insert(b"user:3", b"Charlie".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(20);
        let root = table.root.read().unwrap();
        let bounds = ScanBounds::Prefix(KeyBuf(b"user:".to_vec()));
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(root.as_deref(), &bounds, read_lsn, &mut entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            entries.len(),
            3,
            "Expected 3 entries with prefix 'user:', got {}: {:?}",
            entries.len(),
            entries
                .iter()
                .map(|e| String::from_utf8_lossy(&e.0))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_art_three_then_one() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"aa", b"val1".to_vec(), tx_id);
        table.insert(b"ab", b"val2".to_vec(), tx_id);
        table.insert(b"ac", b"val3".to_vec(), tx_id);

        // Check after 3 inserts
        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }
        let lsn1 = LogSequenceNumber::from(15);
        assert_eq!(
            table.lookup(b"aa", lsn1),
            Some(b"val1".to_vec()),
            "aa after 3 inserts"
        );
        assert_eq!(
            table.lookup(b"ab", lsn1),
            Some(b"val2".to_vec()),
            "ab after 3 inserts"
        );
        assert_eq!(
            table.lookup(b"ac", lsn1),
            Some(b"val3".to_vec()),
            "ac after 3 inserts"
        );

        table.insert(b"zz", b"val4".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(20), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(25);
        let root = table.root.read().unwrap();
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(
            root.as_deref(),
            &ScanBounds::All,
            read_lsn,
            &mut entries,
        );
        eprintln!(
            "Entries after 4 inserts: {:?}",
            entries
                .iter()
                .map(|e| String::from_utf8_lossy(&e.0))
                .collect::<Vec<_>>()
        );

        assert_eq!(
            table.lookup(b"aa", read_lsn),
            Some(b"val1".to_vec()),
            "aa after 4 inserts"
        );
        assert_eq!(
            table.lookup(b"ab", read_lsn),
            Some(b"val2".to_vec()),
            "ab after 4 inserts"
        );
        assert_eq!(
            table.lookup(b"ac", read_lsn),
            Some(b"val3".to_vec()),
            "ac after 4 inserts"
        );
        assert_eq!(
            table.lookup(b"zz", read_lsn),
            Some(b"val4".to_vec()),
            "zz after 4 inserts"
        );
    }

    #[test]
    fn test_art_two_prefixes() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"abc", b"val1".to_vec(), tx_id);
        table.insert(b"xyz", b"val2".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(20);
        let root = table.root.read().unwrap();
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(
            root.as_deref(),
            &ScanBounds::All,
            read_lsn,
            &mut entries,
        );
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            entries.len(),
            2,
            "Expected 2 entries, got {}: {:?}",
            entries.len(),
            entries
        );
        assert_eq!(table.lookup(b"abc", read_lsn), Some(b"val1".to_vec()));
        assert_eq!(table.lookup(b"xyz", read_lsn), Some(b"val2".to_vec()));
    }

    #[test]
    fn test_art_collect_keys_with_order() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"user:1", b"Alice".to_vec(), tx_id);
        table.insert(b"user:2", b"Bob".to_vec(), tx_id);
        table.insert(b"user:3", b"Charlie".to_vec(), tx_id);
        table.insert(b"order:1", b"Order1".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(20);
        let root = table.root.read().unwrap();
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(
            root.as_deref(),
            &ScanBounds::All,
            read_lsn,
            &mut entries,
        );
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            entries.len(),
            4,
            "Expected 4 entries total, got {}: {:?}",
            entries.len(),
            entries
                .iter()
                .map(|e| String::from_utf8_lossy(&e.0))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_art_collect_keys() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"user:1", b"Alice".to_vec(), tx_id);
        table.insert(b"user:2", b"Bob".to_vec(), tx_id);
        table.insert(b"user:3", b"Charlie".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(20);
        let root = table.root.read().unwrap();
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(
            root.as_deref(),
            &ScanBounds::All,
            read_lsn,
            &mut entries,
        );
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_art_prefix_scan_direct() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);

        table.insert(b"user:1", b"Alice".to_vec(), tx_id);
        table.insert(b"user:2", b"Bob".to_vec(), tx_id);
        table.insert(b"user:3", b"Charlie".to_vec(), tx_id);
        table.insert(b"order:1", b"Order1".to_vec(), tx_id);

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        let mut cursor = reader.scan_prefix(b"user:", read_lsn).unwrap();

        let mut count = 0;
        while cursor.valid() {
            count += 1;
            assert!(cursor.key().unwrap().starts_with(b"user:"));
            cursor.next().unwrap();
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn test_art_reader_get() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"key1", b"value1").unwrap();
        writer.put(b"key2", b"value2").unwrap();
        writer.flush().unwrap();

        {
            let mut root = table.root.write().unwrap();
            MemoryART::commit_all(&mut root, LogSequenceNumber::from(10), tx_id);
        }

        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        assert_eq!(
            reader.get(b"key1", read_lsn).unwrap(),
            Some(ValueBuf(b"value1".to_vec()))
        );
        assert_eq!(
            reader.get(b"key2", read_lsn).unwrap(),
            Some(ValueBuf(b"value2".to_vec()))
        );
    }

    #[test]
    fn test_art_basic_put_get() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"key1", b"value1").unwrap();
        writer.put(b"key2", b"value2").unwrap();
        writer.flush().unwrap();
        commit_table(&table, LogSequenceNumber::from(10), tx_id);

        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        assert_eq!(
            reader.get(b"key1", read_lsn).unwrap(),
            Some(ValueBuf(b"value1".to_vec()))
        );
        assert_eq!(
            reader.get(b"key2", read_lsn).unwrap(),
            Some(ValueBuf(b"value2".to_vec()))
        );
        assert_eq!(reader.get(b"key3", read_lsn).unwrap(), None);
    }

    #[test]
    fn test_art_prefix_scan() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"user:1", b"Alice").unwrap();
        writer.put(b"user:2", b"Bob").unwrap();
        writer.put(b"user:3", b"Charlie").unwrap();
        writer.put(b"order:1", b"Order1").unwrap();
        writer.flush().unwrap();
        commit_table(&table, LogSequenceNumber::from(10), tx_id);

        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        let mut cursor = reader.scan_prefix(b"user:", read_lsn).unwrap();

        let mut count = 0;
        while cursor.valid() {
            count += 1;
            assert!(cursor.key().unwrap().starts_with(b"user:"));
            cursor.next().unwrap();
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn test_art_delete() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"key1", b"value1").unwrap();
        writer.put(b"key2", b"value2").unwrap();
        writer.flush().unwrap();
        commit_table(&table, LogSequenceNumber::from(10), tx_id);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.delete(b"key1").unwrap();
        writer.flush().unwrap();

        let read_lsn = LogSequenceNumber::from(30);
        let reader = table.reader(read_lsn).unwrap();
        assert_eq!(reader.get(b"key1", read_lsn).unwrap(), None);
        assert_eq!(
            reader.get(b"key2", read_lsn).unwrap(),
            Some(ValueBuf(b"value2".to_vec()))
        );
    }

    #[test]
    fn test_art_node_growth() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        for i in 0..20 {
            let key = format!("key{:02}", i);
            writer.put(key.as_bytes(), b"value").unwrap();
        }
        writer.flush().unwrap();
        commit_table(&table, LogSequenceNumber::from(10), tx_id);

        // Check count
        let root = table.root.read().unwrap();
        let count = MemoryART::count_leaves(root.as_deref());
        eprintln!("Leaf count after 20 inserts: {}", count);
        assert_eq!(count, 20);

        // Collect all keys
        let mut entries = Vec::new();
        MemoryART::collect_keys_in_bounds(
            root.as_deref(),
            &ScanBounds::All,
            LogSequenceNumber::from(u64::MAX),
            &mut entries,
        );
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        eprintln!(
            "Keys in tree: {:?}",
            entries
                .iter()
                .map(|e| String::from_utf8_lossy(&e.0))
                .collect::<Vec<_>>()
        );
        drop(root);

        // Check with direct lookup
        let read_lsn = LogSequenceNumber::from(20);
        for i in 0..20 {
            let key = format!("key{:02}", i);
            let result = table.lookup(key.as_bytes(), read_lsn);
            eprintln!(
                "Direct lookup {}: {} (visible={})",
                key,
                result.is_some(),
                result.is_some()
            );
        }
        assert_eq!(
            table.lookup(b"key00", read_lsn),
            Some(b"value".to_vec()),
            "Direct lookup key00"
        );
        assert_eq!(
            table.lookup(b"key10", read_lsn),
            Some(b"value".to_vec()),
            "Direct lookup key10"
        );
        assert_eq!(
            table.lookup(b"key19", read_lsn),
            Some(b"value".to_vec()),
            "Direct lookup key19"
        );

        // Check with reader
        let reader = table.reader(read_lsn).unwrap();
        for i in 0..20 {
            let key = format!("key{:02}", i);
            let result = reader.get(key.as_bytes(), read_lsn).unwrap();
            assert_eq!(
                result,
                Some(ValueBuf(b"value".to_vec())),
                "Reader lookup {}",
                key
            );
        }
    }

    #[test]
    fn test_art_capabilities() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let caps = table.capabilities();
        assert!(caps.ordered);
        assert!(caps.point_lookup);
        assert!(caps.prefix_scan);
        assert!(caps.reverse_scan);
        assert!(caps.range_delete);
        assert!(caps.memory_resident);
        assert!(!caps.disk_resident);
    }

    #[test]
    fn test_art_cursor_iteration() {
        let table = MemoryART::new(TableId::from(1), "test_art".to_string());
        let tx_id = TransactionId::from(1);
        let write_lsn = LogSequenceNumber::from(1);

        let mut writer = table.writer(tx_id, write_lsn).unwrap();
        writer.put(b"a", b"1").unwrap();
        writer.put(b"b", b"2").unwrap();
        writer.put(b"c", b"3").unwrap();
        writer.flush().unwrap();
        commit_table(&table, LogSequenceNumber::from(10), tx_id);

        let read_lsn = LogSequenceNumber::from(20);
        let reader = table.reader(read_lsn).unwrap();
        let mut cursor = reader.scan(ScanBounds::All, read_lsn).unwrap();

        assert!(cursor.valid());
        assert_eq!(cursor.key(), Some(b"a".as_ref()));
        cursor.next().unwrap();
        assert_eq!(cursor.key(), Some(b"b".as_ref()));
        cursor.next().unwrap();
        assert_eq!(cursor.key(), Some(b"c".as_ref()));
        cursor.next().unwrap();
        assert!(!cursor.valid());
    }
}
