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

//! Tokenizer for full-text search.
//!
//! Supports multiple tokenization strategies:
//! - Whitespace: Simple whitespace splitting
//! - Simple: Whitespace + lowercase + punctuation removal
//! - Stemming: Simple + basic English stemming

use std::collections::HashSet;

/// Tokenizer type for full-text search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TokenizerKind {
    /// Simple whitespace splitting
    Whitespace,
    /// Whitespace + lowercase + punctuation removal
    #[default]
    Simple,
    /// Simple + basic English stemming
    Stemming,
}

/// Configuration for tokenization.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TokenizerConfig {
    /// Tokenizer kind
    pub kind: TokenizerKind,
    /// Minimum word length (shorter words are ignored)
    pub min_word_length: usize,
    /// Stop words to exclude
    pub stop_words: HashSet<String>,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            kind: TokenizerKind::Simple,
            min_word_length: 2,
            stop_words: DEFAULT_STOP_WORDS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Default English stop words.
const DEFAULT_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

/// Tokenizer for full-text search.
pub struct Tokenizer {
    config: TokenizerConfig,
}

impl Tokenizer {
    /// Create a new tokenizer with the given configuration.
    pub fn new(config: TokenizerConfig) -> Self {
        Self { config }
    }

    /// Create a new tokenizer with default configuration.
    pub fn default() -> Self {
        Self {
            config: TokenizerConfig::default(),
        }
    }

    /// Tokenize text into a list of terms.
    ///
    /// Returns a vector of (term, position) pairs.
    pub fn tokenize(&self, text: &str) -> Vec<(String, usize)> {
        let mut terms = Vec::new();
        let mut position = 0;

        match self.config.kind {
            TokenizerKind::Whitespace => {
                for word in text.split_whitespace() {
                    if word.len() >= self.config.min_word_length {
                        terms.push((word.to_string(), position));
                        position += 1;
                    }
                }
            }
            TokenizerKind::Simple => {
                for word in text.split_whitespace() {
                    let normalized = self.normalize_word(word);
                    if normalized.len() >= self.config.min_word_length
                        && !self.config.stop_words.contains(&normalized)
                    {
                        terms.push((normalized, position));
                        position += 1;
                    }
                }
            }
            TokenizerKind::Stemming => {
                for word in text.split_whitespace() {
                    let normalized = self.normalize_word(word);
                    if normalized.len() >= self.config.min_word_length
                        && !self.config.stop_words.contains(&normalized)
                    {
                        let stemmed = self.stem(&normalized);
                        terms.push((stemmed, position));
                        position += 1;
                    }
                }
            }
        }

        terms
    }

    /// Normalize a word: lowercase and remove punctuation.
    fn normalize_word(&self, word: &str) -> String {
        word.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    }

    /// Basic English stemming (Porter stemmer simplified).
    fn stem(&self, word: &str) -> String {
        let word = word.to_lowercase();

        // Step 1a: Plurals
        let word = if word.ends_with("sses") {
            word[..word.len() - 2].to_string()
        } else if word.ends_with("ies") {
            if word.len() > 4 {
                word[..word.len() - 3].to_string()
            } else {
                word
            }
        } else if word.ends_with('s') && !word.ends_with("ss") {
            word[..word.len() - 1].to_string()
        } else {
            word
        };

        // Step 1b: -ed, -ing
        let word = if word.ends_with("eed") {
            if word.len() > 4 {
                word[..word.len() - 1].to_string()
            } else {
                word
            }
        } else if word.ends_with("ed") && word.len() > 4 {
            let base = &word[..word.len() - 2];
            if base
                .chars()
                .any(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
            {
                base.to_string()
            } else {
                word
            }
        } else if word.ends_with("ing") && word.len() > 5 {
            let base = &word[..word.len() - 3];
            if base
                .chars()
                .any(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
            {
                base.to_string()
            } else {
                word
            }
        } else {
            word
        };

        // Step 1c: -y -> -i
        let word = if word.ends_with('y') && word.len() > 2 {
            let base = &word[..word.len() - 1];
            if base
                .chars()
                .any(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
            {
                format!("{}i", base)
            } else {
                word
            }
        } else {
            word
        };

        // Common suffixes
        let suffixes = ["tion", "ness", "ment", "able", "ible", "less", "ful"];
        for suffix in &suffixes {
            if word.ends_with(suffix) && word.len() > suffix.len() + 2 {
                return word[..word.len() - suffix.len()].to_string();
            }
        }

        word
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_whitespace_tokenizer() {
        let config = TokenizerConfig {
            kind: TokenizerKind::Whitespace,
            min_word_length: 1,
            stop_words: HashSet::new(),
        };
        let tokenizer = Tokenizer::new(config);
        let terms = tokenizer.tokenize("hello world foo");
        assert_eq!(terms.len(), 3);
        assert_eq!(terms[0], ("hello".to_string(), 0));
        assert_eq!(terms[1], ("world".to_string(), 1));
        assert_eq!(terms[2], ("foo".to_string(), 2));
    }

    #[test]
    fn test_simple_tokenizer() {
        let tokenizer = Tokenizer::default();
        let terms = tokenizer.tokenize("Hello World! The Foo");
        assert_eq!(terms.len(), 3);
        assert_eq!(terms[0], ("hello".to_string(), 0));
        assert_eq!(terms[1], ("world".to_string(), 1));
        assert_eq!(terms[2], ("foo".to_string(), 2));
        // "the" should be filtered as stop word
    }

    #[test]
    fn test_stemming_tokenizer() {
        let config = TokenizerConfig {
            kind: TokenizerKind::Stemming,
            min_word_length: 2,
            stop_words: HashSet::new(),
        };
        let tokenizer = Tokenizer::new(config);
        let terms = tokenizer.tokenize("running jumped cats");
        assert_eq!(terms.len(), 3);
        assert!(terms[0].0.contains("run"));
        assert!(terms[1].0.contains("jump"));
        assert!(terms[2].0.contains("cat"));
    }

    #[test]
    fn test_min_word_length() {
        let config = TokenizerConfig {
            kind: TokenizerKind::Whitespace,
            min_word_length: 4,
            stop_words: HashSet::new(),
        };
        let tokenizer = Tokenizer::new(config);
        let terms = tokenizer.tokenize("the quick brown fox");
        assert_eq!(terms.len(), 2);
        // "the" and "fox" filtered (length < 4)
    }
}
