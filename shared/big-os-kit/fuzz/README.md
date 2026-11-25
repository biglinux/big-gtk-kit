# Memory-input fuzzing

These targets parse supplied bytes without changing the environment, reading a
cgroup, or connecting to a pressure publisher. `cgroup_capacity` includes the
private production parser so no public test API is needed. Its fields are
membership, mountinfo and control value separated by `|`; `pressure_config`
uses a path and optional base64 trigger separated by `|`. `microdaemon_frame`
takes the bytes a peer writes to a daemon's socket: a big-endian length, then
JSON.

From this directory, with cargo-fuzz and a nightly toolchain satisfying the
parent crate's MSRV (currently Rust 1.100.0):

```sh
mkdir -p corpus/cgroup_capacity corpus/pressure_config corpus/microdaemon_frame
cp seeds/cgroup_capacity/* corpus/cgroup_capacity/
cp seeds/pressure_config/* corpus/pressure_config/
cp seeds/microdaemon_frame/* corpus/microdaemon_frame/
cargo +nightly fuzz run cgroup_capacity -- -max_total_time=60 -max_len=65536
cargo +nightly fuzz run pressure_config -- -max_total_time=60 -max_len=131072
ASAN_OPTIONS=quarantine_size_mb=16 cargo +nightly fuzz run microdaemon_frame -- -max_total_time=300 -rss_limit_mb=256
```

AddressSanitizer keeps 256 MB of freed memory by default, which alone passes
the 256 MB limit; `quarantine_size_mb=16` leaves room for the target.

`seeds/` is versioned; the evolving `corpus/` and build output are not. Keep any
crash or hang as a named regression fixture in the ordinary parser tests. These
bounded fuzz runs are separate from the per-commit gate.
