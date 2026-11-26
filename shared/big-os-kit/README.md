# big-os-kit

Operating-system primitives with no GTK: atomic and versioned storage,
settings, paths, subprocesses, the user database, Samba shares and memory
limits. Code one program alone needs stays in that program.

## Main entry points

- `storage`: atomic and versioned JSON documents, private files, file locks.
- `settings`: settings documents written on a GIO worker, and the XDG folders.
- `subprocess`: programs run by argv with a deadline, cancellation and limits.
- `file_io`: bounded reads, regular-file opens and private staging folders.
- `http_client`: HTTP and HTTPS through `curl`.

Use `serde_path` on persisted `PathBuf` fields (`serde_path::optional` for
nullable fields). UTF-8 paths remain JSON strings; other paths keep their
native bytes through Serde's platform representation.

A thread other than the main one that draws with cairo or Pango holds
`exit_wait::drawing()` while it draws, so the process does not exit under it.

The public API and its safety requirements are in the crate's rustdoc.

## Ownership and lifecycle

Values and worker operations are GTK-free; callers own threads, tasks and
the person's interaction.

## Example

`cargo run -p big-os-kit --example versioned_json`.

## Validate

```bash
cargo fmt -p big-os-kit -- --check
cargo clippy -p big-os-kit --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-os-kit --all-features --locked
cargo test -p big-os-kit --all-features --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK.

### Memory sizing and cgroups

`system_memory::total_memory_bytes()` is the physical `MemTotal`, for display.
The sizing baseline the crate's budgets use is the smallest of physical RAM
and the finite `memory.high` and `memory.max` across the process's visible
cgroup v2 ancestors, read once. It resolves `/proc/self/cgroup` through
`mountinfo` rather than assuming the cgroup mount root is the process's leaf,
and changes no kernel setting.

An unavailable control is not zero, `max` is unlimited, and a v1 or unreadable
hierarchy without a known finite limit sizes conservatively. A missing or
deleted leaf is not read as an unrestricted group. Hidden ancestors, a
migration, shared consumption and limits changed later keep this from
guaranteeing an allocation: `memory.high` throttles, it reserves nothing.
