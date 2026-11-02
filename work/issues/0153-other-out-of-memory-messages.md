---
status: todo
type: debt
created: 2026-11-02
---
# Two more DuckDB out-of-memory messages are not in the list

## Problem
`mallard_duck::is_out_of_memory` lists the messages of DuckDB's buffer manager (issue 0131). DuckDB has two more
`OutOfMemoryException`s that a write can meet: `Failed to re-allocate block of %llu bytes (bad allocation)`
(`common/allocator/allocator.cpp`) and `failed to offload data block of size %s` (`storage/temporary_file_manager.cpp`,
the limit on temporary files). The appender can pass messages on without the "Out of Memory Error" type. Such an
error would then count as permanent on the OTel side and drop the rows.

Not seen in a run; from reading DuckDB's source.

## Notes
Probably two more entries. The second is not short for a moment like the others, so check that retrying it is right.
