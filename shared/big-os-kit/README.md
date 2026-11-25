# big-os-kit

GTK-free OS primitives: atomic and versioned storage, paths, processes, the user
database and Samba shares.

## Use when

Use this crate to reuse an established display-free operating-system primitive with a durable tested contract.

## Do not use when

Do not use it as a generic dumping ground; product-only helpers stay in their product.

## Main entry points

atomic/versioned JSON storage, settings streams, process specs, diagnostics, paths and Linux file helpers.

Use `serde_path` on persisted `PathBuf` fields (`serde_path::optional` for
nullable fields). UTF-8 paths remain JSON strings; other paths retain their
native bytes through Serde's platform representation. BigTerminal session and
profile paths and `big-remote-fs` identity files use this adapter.

A thread other than the main one that draws with cairo or Pango holds
`exit_wait::drawing()` while it draws, so the process does not exit under it.

The exact public API and safety requirements are in crate-level rustdoc. Do not
infer current methods from roadmap documents.

## Ownership and lifecycle

Values and worker operations are GTK-free; callers own threads/tasks and user interaction.

## Native dependencies

Linux userspace tools only in the modules that explicitly invoke them.

The workspace feature gates and nearest product PKGBUILD define exact versions.
See [getting started](https://github.com/biglinux/big/blob/main/docs/handbook/getting-started.md).

## Example

`cargo run -p big-os-kit --example versioned_json`.

The complete learning map is [framework examples](https://github.com/biglinux/big/blob/main/docs/reference/framework-examples.md).

## Consumers

bigdesktop's [package catalog](https://github.com/biglinux/big/blob/main/docs/reference/packages.md) lists its packages that use this one.

## Validate

```bash
cargo fmt -p big-os-kit -- --check
cargo clippy -p big-os-kit --all-targets --all-features --locked -- -D warnings
cargo nextest run -p big-os-kit --locked
cargo test -p big-os-kit --doc --locked
```

Run graphical tests through `tools/headless-gate.sh` when the changed behavior
needs GTK. Follow [validation](https://github.com/biglinux/big/blob/main/docs/handbook/validation.md) for the full matrix.

### Memory sizing and cgroups

`system_memory::total_memory_bytes()` remains physical `MemTotal` for display.
`effective_memory_bytes()` is a cached sizing baseline: the minimum observed
physical RAM and finite `memory.high`/`memory.max` across the process's visible
cgroup v2 ancestors. It resolves `/proc/self/cgroup` through `mountinfo` rather
than assuming the cgroup mount root is the process's leaf. No kernel settings
are modified and no polling thread is created.

`read_capacity()` exposes the individual observations and their completeness.
Unavailable controls are not zero, `max` is unlimited, and unsupported v1 or
unreadable topology without a known finite constraint uses conservative sizing.
A missing/deleted leaf is not treated as an unrestricted group. Hidden namespace
ancestors, concurrent cgroup migration, shared consumption and limits changed
later prevent this observation from guaranteeing an allocation. Keep pressure
monitoring; `memory.high` is a throttling boundary, not a hard reservation.

Session payload allowances retain the 4 GiB reference profile but scale down to
at most one sixteenth of a smaller observed capacity. CPU/GPU-estimate ledgers
remain separate and must not be added to PSS as disjoint physical allocations.
