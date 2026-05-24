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

//! NanoStore - A high-performance embedded database engine
//!
//! NanoStore is a transactional, ACID-compliant embedded database engine designed for
//! high-performance data storage and retrieval. It provides multiple specialized table types
//! including B-trees, LSM trees, hash tables, R-trees for geospatial data, HNSW for vector
//! similarity search, and more.
//!
//! # Features
//!
//! - **ACID Transactions**: Full MVCC support with snapshot isolation
//! - **Multiple Table Types**: B-tree, LSM, Hash, R-tree, HNSW, Graph, TimeSeries, and more
//! - **Write-Ahead Logging**: Crash recovery and durability guarantees
//! - **Flexible Storage**: Pluggable VFS layer supporting local filesystem and in-memory storage
//! - **Compression & Encryption**: Built-in support for data compression and encryption
//! - **Concurrent Access**: Lock-free data structures and optimistic concurrency control
//!
//! # Example
//!
//! ```no_run
//! use nanostore::engine::StorageEngine;
//! use nanostore::vfs::LocalFileSystem;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a new storage engine
//! let vfs = LocalFileSystem::new();
//! let engine = StorageEngine::open(vfs, "mydb.db")?;
//!
//! // Use the engine...
//! # Ok(())
//! # }
//! ```

#![warn(
    clippy::cargo,
    missing_docs,
    clippy::pedantic,
    future_incompatible,
    rust_2018_idioms
)]
#![allow(
    clippy::option_if_let_else,
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    clippy::type_complexity,
    clippy::cast_ptr_alignment,
    clippy::multiple_crate_versions,
    clippy::large_enum_variant,
    clippy::inefficient_to_string,
    clippy::enum_variant_names,
    clippy::trivially_copy_pass_by_ref,
    clippy::needless_pass_by_value,
    clippy::match_same_arms,
    clippy::wildcard_enum_match_arm,
    clippy::manual_let_else,
    clippy::used_underscore_binding,
    clippy::needless_continue,
    clippy::items_after_statements,
    clippy::unnecessary_wraps,
    clippy::must_use_candidate,
    clippy::if_same_then_else,
    clippy::unnecessary_debug_formatting,
    clippy::ptr_arg,
    clippy::format_collect,
    clippy::default_trait_access,
    clippy::field_reassign_with_default,
    clippy::implicit_clone,
    clippy::needless_for_each,
    clippy::assigning_clones,
    clippy::unused_self,
    clippy::needless_range_loop,
    clippy::match_wildcard_for_single_variants,
    clippy::should_implement_trait,
    clippy::unnecessary_sort_by,
    clippy::no_effect_underscore_binding,
    clippy::must_use_unit,
    clippy::struct_excessive_bools,
    dead_code,
    unused_assignments,
    unused_must_use
)]

/// Storage engine implementation providing database operations and table management
pub mod engine;
/// Error types used throughout the crate
pub mod error;
/// Page-based storage management with caching and overflow handling
pub mod pager;
mod rest;
/// Snapshot isolation and MVCC support
pub mod snap;
/// Table implementations (B-tree, LSM, Hash, R-tree, HNSW, Graph, TimeSeries, etc.)
pub mod table;
/// Transaction management and conflict detection
pub mod txn;
/// Core type definitions and utilities
pub mod types;
/// Virtual file system abstraction layer
pub mod vfs;
/// Write-ahead logging for durability and crash recovery
pub mod wal;

/// Commonly used types and traits for convenient imports
pub mod prelude {}

// Re-export metrics and tracing for convenience
pub use metrics;
pub use tracing;
pub use tracing_timing;
