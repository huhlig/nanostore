# Architecture

Nanostore is designed as a paged single file multi-table key value database.

VFS -> Pager -> Table -> API

# Modules
* VFS is a filesystem abstraction.
* WAL is a write ahead log implementation for checkpoining the pager file.
* Pager is a single file block format for storing ordered pages of data.
* Table is a collection of table implementations that use the Pager format.
* Index is a collection of index implementations that use the pager format.
* Cache is a collection of cache implementations for tables and indexes.
* API is the core API for table management and usage.
* Rest is a simple Network Handler.
* Bin is a cli wrapper for table management and network service.

