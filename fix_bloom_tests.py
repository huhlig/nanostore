#!/usr/bin/env python3
"""Fix bloom filter test calls to include tx_id and lsn parameters."""

import re
import sys

def fix_bloom_insert_calls(content):
    """Replace filter.insert(key) with filter.insert(key, TransactionId::from(1), LogSequenceNumber::from(1))"""
    # Pattern to match filter.insert(something).unwrap()
    pattern = r'filter\.insert\(([^)]+)\)\.unwrap\(\)'
    replacement = r'filter.insert(\1, TransactionId::from(1), LogSequenceNumber::from(1)).unwrap()'
    return re.sub(pattern, replacement, content)

def main():
    if len(sys.argv) != 2:
        print("Usage: python fix_bloom_tests.py <file>")
        sys.exit(1)
    
    filepath = sys.argv[1]
    
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()
    
    fixed_content = fix_bloom_insert_calls(content)
    
    with open(filepath, 'w', encoding='utf-8') as f:
        f.write(fixed_content)
    
    print(f"Fixed {filepath}")

if __name__ == '__main__':
    main()

# Made with Bob
