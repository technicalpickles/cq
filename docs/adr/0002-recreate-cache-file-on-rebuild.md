# Recreate the cache file on rebuild, at the latest storage format

**Status:** accepted

cq opens `index.duckdb` with `storage_compatibility_version = 'latest'`, and any rebuild (schema version mismatch, `--reindex`, or a file whose `storage_version` tag isn't `STORAGE_VERSION_TAG`) deletes the file and creates a fresh one instead of dropping tables in place.

## Why

- DuckDB defaults new files to the v0.10.2 storage format for backwards compatibility. That format disables the ZSTD and DICT_FSST string codecs, so `raw_records.json` (median record 2 KB, p99 59 KB) was stored uncompressed. On a real 6.1 GB cache, moving to the current format cut that column from ~4 GiB to 440 MiB with no read-time regression.
- Storage format is fixed at file creation, and DuckDB doesn't return free blocks to the OS. Dropping tables in place kept both problems forever; recreating fixes both.
- It's safe because the cache only mirrors transcript files still on disk (`do_sync` deletes rows for files that are gone), so a rebuild loses nothing.

## Consequences

- DuckDB CLIs older than 1.5 can't open the cache file. Only cq reads it.
- A DuckDB upgrade that changes the storage tag makes `fresh_cache_uses_current_storage_version` fail. Updating `STORAGE_VERSION_TAG` recreates every existing cache once, which costs one full reindex.
- If cq starts retaining sessions whose transcripts were pruned ([#64](https://github.com/technicalpickles/cq/issues/64)), a rebuild would destroy them. That work has to carry retained data across the recreate (or store it outside `index.duckdb`) before it ships.
- Recreation is serialized through `index.lock`, with a re-check once the lock is held, because there's a window with no DuckDB connection open where another cq process could open or delete the file.
- Known gap: a `--reindex` racing another cq invocation within milliseconds can unlink the file under a reader that already opened it, since the fast path for a current file takes no lock. Bounded to cache data and user-triggered; closing it would mean making every open take a lock and block behind syncs, which isn't worth it.
