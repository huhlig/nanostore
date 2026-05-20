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

//! Group commit optimization for WAL
//!
//! This module implements group commit, which batches multiple transaction commits
//! into a single fsync operation to improve throughput under high concurrency.

use crate::txn::TransactionId;
use crate::wal::{LogSequenceNumber, WalResult};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Configuration for group commit
#[derive(Debug, Clone)]
pub struct GroupCommitConfig {
    /// Enable group commit optimization
    pub enabled: bool,
    /// Maximum number of commits to batch together
    pub max_batch_size: usize,
    /// Maximum time to wait for batching (microseconds)
    pub max_wait_micros: u64,
    /// Minimum batch size to trigger early flush
    pub min_batch_size: usize,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_batch_size: 100,
            max_wait_micros: 1000, // 1ms
            min_batch_size: 10,
        }
    }
}

impl GroupCommitConfig {
    /// Create a configuration optimized for high throughput
    pub fn high_throughput() -> Self {
        Self {
            enabled: true,
            max_batch_size: 100,
            max_wait_micros: 1000,
            min_batch_size: 10,
        }
    }

    /// Create a configuration optimized for low latency
    pub fn low_latency() -> Self {
        Self {
            enabled: false,
            max_batch_size: 1,
            max_wait_micros: 0,
            min_batch_size: 1,
        }
    }

    /// Create a balanced configuration
    pub fn balanced() -> Self {
        Self {
            enabled: true,
            max_batch_size: 50,
            max_wait_micros: 500,
            min_batch_size: 5,
        }
    }
}

/// Metrics for group commit effectiveness
#[derive(Debug, Default)]
pub struct GroupCommitMetrics {
    /// Total number of commits processed
    pub total_commits: AtomicU64,
    /// Total number of fsync operations
    pub total_fsyncs: AtomicU64,
    /// Total number of batches
    pub total_batches: AtomicU64,
    /// Maximum batch size seen
    pub max_batch_size: AtomicU64,
    /// Total wait time (microseconds)
    pub total_wait_time_us: AtomicU64,
}

impl GroupCommitMetrics {
    /// Create new metrics
    pub fn new() -> Self {
        Self::default()
    }

    /// Get average batch size
    pub fn avg_batch_size(&self) -> f64 {
        let batches = self.total_batches.load(Ordering::Relaxed);
        if batches == 0 {
            return 0.0;
        }
        let commits = self.total_commits.load(Ordering::Relaxed);
        commits as f64 / batches as f64
    }

    /// Get average wait time (microseconds)
    pub fn avg_wait_time_us(&self) -> f64 {
        let commits = self.total_commits.load(Ordering::Relaxed);
        if commits == 0 {
            return 0.0;
        }
        let total_wait = self.total_wait_time_us.load(Ordering::Relaxed);
        total_wait as f64 / commits as f64
    }

    /// Get fsync reduction ratio
    pub fn fsync_reduction_ratio(&self) -> f64 {
        let commits = self.total_commits.load(Ordering::Relaxed);
        let fsyncs = self.total_fsyncs.load(Ordering::Relaxed);
        if fsyncs == 0 {
            return 0.0;
        }
        commits as f64 / fsyncs as f64
    }

    /// Record a batch
    pub fn record_batch(&self, batch_size: usize, wait_time_us: u64) {
        self.total_commits
            .fetch_add(batch_size as u64, Ordering::Relaxed);
        self.total_batches.fetch_add(1, Ordering::Relaxed);
        self.total_fsyncs.fetch_add(1, Ordering::Relaxed);
        self.total_wait_time_us
            .fetch_add(wait_time_us * batch_size as u64, Ordering::Relaxed);

        // Update max batch size
        let mut current_max = self.max_batch_size.load(Ordering::Relaxed);
        while batch_size as u64 > current_max {
            match self.max_batch_size.compare_exchange_weak(
                current_max,
                batch_size as u64,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_max = x,
            }
        }
    }
}

/// Result of a commit operation (simplified for Send)
#[derive(Debug, Clone)]
pub enum CommitResult {
    /// Commit succeeded
    Success,
    /// Commit failed with error message
    Error(String),
}

/// A commit request waiting in the queue
pub struct CommitRequest {
    /// Transaction ID
    pub txn_id: TransactionId,
    /// LSN of the commit record
    pub lsn: LogSequenceNumber,
    /// Time when request was queued
    pub queued_at: Instant,
    /// Channel to notify when commit completes
    pub notifier: crossbeam_channel::Sender<CommitResult>,
}

/// Queue of pending commit requests
struct CommitQueue {
    /// Pending commit requests
    pending: VecDeque<CommitRequest>,
    /// Configuration
    config: GroupCommitConfig,
}

impl CommitQueue {
    /// Create a new commit queue
    fn new(config: GroupCommitConfig) -> Self {
        Self {
            pending: VecDeque::new(),
            config,
        }
    }

    /// Add a commit request to the queue
    fn enqueue(&mut self, request: CommitRequest) {
        self.pending.push_back(request);
    }

    /// Check if the queue should be flushed
    fn should_flush(&self, last_flush: Instant) -> bool {
        if self.pending.is_empty() {
            return false;
        }

        // Flush if batch is full
        if self.pending.len() >= self.config.max_batch_size {
            return true;
        }

        let elapsed = last_flush.elapsed();
        let max_wait = Duration::from_micros(self.config.max_wait_micros);

        // Flush if minimum batch size reached and timeout expired
        if self.pending.len() >= self.config.min_batch_size && elapsed >= max_wait {
            return true;
        }

        // Flush if timeout expired
        if elapsed >= max_wait {
            return true;
        }

        false
    }

    /// Drain all pending requests
    fn drain(&mut self) -> Vec<CommitRequest> {
        self.pending.drain(..).collect()
    }

    /// Get the number of pending requests
    fn len(&self) -> usize {
        self.pending.len()
    }
}

/// Coordinator for group commit operations
pub struct GroupCommitCoordinator {
    /// Commit queue
    queue: Arc<Mutex<CommitQueue>>,
    /// Metrics
    metrics: Arc<GroupCommitMetrics>,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Worker thread handle
    worker_handle: Option<JoinHandle<()>>,
    /// Flush callback
    flush_callback: Arc<dyn Fn() -> WalResult<()> + Send + Sync>,
}

impl GroupCommitCoordinator {
    /// Create a new group commit coordinator
    pub fn new<F>(config: GroupCommitConfig, flush_callback: F) -> Self
    where
        F: Fn() -> WalResult<()> + Send + Sync + 'static,
    {
        let queue = Arc::new(Mutex::new(CommitQueue::new(config.clone())));
        let metrics = Arc::new(GroupCommitMetrics::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let flush_callback = Arc::new(flush_callback);

        let mut coordinator = Self {
            queue: queue.clone(),
            metrics: metrics.clone(),
            shutdown: shutdown.clone(),
            worker_handle: None,
            flush_callback: flush_callback.clone(),
        };

        // Start worker thread if group commit is enabled
        if config.enabled {
            let worker_handle = thread::spawn(move || {
                Self::worker_loop(queue, metrics, shutdown, flush_callback, config);
            });
            coordinator.worker_handle = Some(worker_handle);
        }

        coordinator
    }

    /// Worker thread loop
    fn worker_loop(
        queue: Arc<Mutex<CommitQueue>>,
        metrics: Arc<GroupCommitMetrics>,
        shutdown: Arc<AtomicBool>,
        flush_callback: Arc<dyn Fn() -> WalResult<()> + Send + Sync>,
        config: GroupCommitConfig,
    ) {
        let mut last_flush = Instant::now();
        let sleep_duration = Duration::from_micros(config.max_wait_micros / 10);

        while !shutdown.load(Ordering::Relaxed) {
            // Check if we should flush
            let should_flush = {
                let queue = queue.lock();
                queue.should_flush(last_flush)
            };

            if should_flush {
                // Drain the queue
                let batch = {
                    let mut queue = queue.lock();
                    queue.drain()
                };

                if !batch.is_empty() {
                    let batch_size = batch.len();
                    let flush_start = Instant::now();

                    // Perform the flush
                    let result = flush_callback();

                    // Calculate wait time for metrics
                    let wait_time_us = flush_start.duration_since(last_flush).as_micros() as u64;
                    metrics.record_batch(batch_size, wait_time_us);

                    // Notify all waiting transactions
                    // If flush succeeded, send Success to all. If failed, send Error to all.
                    match result {
                        Ok(()) => {
                            for request in batch {
                                let _ = request.notifier.send(CommitResult::Success);
                            }
                        }
                        Err(e) => {
                            let error_msg = format!("{}", e);
                            for request in batch {
                                let _ = request
                                    .notifier
                                    .send(CommitResult::Error(error_msg.clone()));
                            }
                        }
                    }

                    last_flush = Instant::now();
                }
            } else {
                // Sleep briefly to avoid busy waiting
                thread::sleep(sleep_duration);
            }
        }
    }

    /// Submit a commit request
    pub fn submit_commit(&self, txn_id: TransactionId, lsn: LogSequenceNumber) -> WalResult<()> {
        let (sender, receiver) = crossbeam_channel::bounded(1);

        let request = CommitRequest {
            txn_id,
            lsn,
            queued_at: Instant::now(),
            notifier: sender,
        };

        // Add to queue
        {
            let mut queue = self.queue.lock();
            queue.enqueue(request);
        }

        // Wait for notification
        match receiver.recv() {
            Ok(CommitResult::Success) => Ok(()),
            Ok(CommitResult::Error(msg)) => Err(crate::wal::WalError::commit_notification_error(
                txn_id, lsn, msg,
            )),
            Err(_) => Err(crate::wal::WalError::commit_notification_error(
                txn_id,
                lsn,
                "Commit notification channel closed",
            )),
        }
    }

    /// Get metrics
    pub fn metrics(&self) -> &GroupCommitMetrics {
        &self.metrics
    }

    /// Shutdown the coordinator
    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);

        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }

        // Flush any remaining requests
        let remaining = {
            let mut queue = self.queue.lock();
            queue.drain()
        };

        // Notify remaining requests with error
        for request in remaining {
            let _ = request
                .notifier
                .send(CommitResult::Error("Coordinator shutdown".to_string()));
        }
    }
}

impl Drop for GroupCommitCoordinator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_commit_config() {
        let config = GroupCommitConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_batch_size, 100);

        let high_throughput = GroupCommitConfig::high_throughput();
        assert!(high_throughput.enabled);

        let low_latency = GroupCommitConfig::low_latency();
        assert!(!low_latency.enabled);
    }

    #[test]
    fn test_metrics() {
        let metrics = GroupCommitMetrics::new();

        metrics.record_batch(10, 1000);
        assert_eq!(metrics.total_commits.load(Ordering::Relaxed), 10);
        assert_eq!(metrics.total_batches.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.total_fsyncs.load(Ordering::Relaxed), 1);

        metrics.record_batch(20, 2000);
        assert_eq!(metrics.total_commits.load(Ordering::Relaxed), 30);
        assert_eq!(metrics.avg_batch_size(), 15.0);
    }

    #[test]
    fn test_commit_queue() {
        let config = GroupCommitConfig::default();
        let queue = CommitQueue::new(config);

        assert_eq!(queue.len(), 0);
        assert!(!queue.should_flush(Instant::now()));
    }

    #[test]
    fn test_queue_should_flush_on_max_batch() {
        let config = GroupCommitConfig {
            enabled: true,
            max_batch_size: 5,
            max_wait_micros: 10000,
            min_batch_size: 2,
        };
        let mut queue = CommitQueue::new(config);

        // Add requests up to max batch size
        for i in 0..5 {
            let (sender, _) = crossbeam_channel::bounded(1);
            queue.enqueue(CommitRequest {
                txn_id: TransactionId::from(i),
                lsn: LogSequenceNumber::from(i),
                queued_at: Instant::now(),
                notifier: sender,
            });
        }

        assert!(queue.should_flush(Instant::now()));
    }

    #[test]
    fn test_queue_should_flush_on_timeout() {
        let config = GroupCommitConfig {
            enabled: true,
            max_batch_size: 100,
            max_wait_micros: 100, // 100 microseconds
            min_batch_size: 2,
        };
        let mut queue = CommitQueue::new(config);

        let (sender, _) = crossbeam_channel::bounded(1);
        queue.enqueue(CommitRequest {
            txn_id: TransactionId::from(1),
            lsn: LogSequenceNumber::from(1),
            queued_at: Instant::now(),
            notifier: sender,
        });

        let last_flush = Instant::now() - Duration::from_micros(200);
        assert!(queue.should_flush(last_flush));
    }
}
