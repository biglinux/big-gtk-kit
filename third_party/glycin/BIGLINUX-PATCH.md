# glycin 3.1.0, patched

A copy of the glycin 3.1.0 library from crates.io (the sandboxed image
decoding under big-image), with these changes:

- `src/pool.rs` drops `use std::usize;`, a module Rust 1.100 deprecates;
  `usize::MAX` reads the same constant.
- `PoolConfig::nice` gives a pool the nice value its loader and editor
  processes start with (`src/pool.rs`), set in the forked process before it
  runs bubblewrap (`src/dbus.rs`), so the loader inside inherits it; inside
  Flatpak the portal starts the loader, which does not inherit it. Upstream
  starts every loader at the priority of the thread that asked for it, and
  glycin asks from the thread that polls the load, the GTK main thread for most
  callers: a folder of thumbnails then decodes at the same priority as the
  desktop's own frames, inside the desktop's cgroup. big-image keeps a pool at
  nice 10 for thumbnails and the global pool for everything else. big-image's
  tests check it (`thumbnail_loader_runs_niced`).
- The pool's cleanup arms itself again for a loader it keeps idle
  (`Pool::clean_loaders`). Upstream arms it only when a loader's last user
  leaves; a loader used after that (a finished load cancelled late) was not
  idle long enough when the cleanup ran, and stayed until the next decode,
  for minutes or the whole session. `pool::tests` checks it
  (`cargo nextest run -p glycin --lib`, one of the workspace's checks).
- The same cleanup releases idle editor processes, which upstream keeps for
  the life of the program: an encoder (big-image's PNG output) or an edit
  started one and it never left.
- The handler a load or an edit connects to the caller's cancellable holds its
  process weakly (`src/api_loader.rs`, `src/api_editor.rs`). Upstream holds it
  strongly, so a caller that keeps its cancellable after the load (a cover art
  request kept until the next track) kept the sandbox running after the pool
  let it go.
- `Loader::accepted_hdr_memory_formats` names the formats a frame whose CICP
  transfer is PQ or HLG may also come in (`src/api_loader.rs`, `src/dbus.rs`).
  Upstream picks the format with one selection for every frame, so a caller
  that wants an HDR picture at 16 bits takes every 10-bit SDR picture at 16
  bits too. big-image's texture decode keeps HDR frames this way.

- A loader started where bubblewrap cannot make a namespace (user
  namespaces off, `SandboxMechanism::NotSandboxed`) runs confined
  (`src/sandbox.rs`, `src/landlock.rs`): it sets on itself, before it executes
  the loader from `/`, the bubblewrap sandbox's memory limit and seccomp
  filter, with `socket`, `connect` and `socketcall` refused, since no network
  namespace keeps a socket from the network and the session bus, `chmod` and
  `fchmodat` refused, since Landlock does not govern a mode change, and with
  `getpriority`, `setpriority`, `prlimit64` and `sched_getaffinity` on itself
  only, since no PID namespace hides the user's other processes. Landlock
  rules let it open what the bubblewrap sandbox holds: `/usr`, the library
  folders, the loader, `/etc/ld.so.cache`, `/dev/null` and the other plain
  devices, the image's folder for a loader that reads beside it, and
  fontconfig's files and cache; and keep it from signaling another process.
  Landlock does not govern `stat`: the loader can still see which paths exist
  and their metadata. A Flatpak development build, which upstream also runs
  without bubblewrap, keeps its loaders under `/app`, which these rules leave
  out: it loads no image. A kernel without Landlock ABI 6 (before Linux 6.12)
  starts no loader: the load fails with `Error::SandboxUnavailable`. Upstream
  runs that loader with nothing but a cleared environment. Such a loader
  also does not start when the forked process cannot mark the descriptors it
  inherited close-on-exec, which upstream ignores: Landlock leaves a
  descriptor opened before it usable. `sandbox::tests` and `landlock::tests`
  check it.
- An ICC conversion builds one lcms2 transform, without its last-pixel cache
  so the threads can share it, and starts a thread per 131 072 pixels, up to
  one per core (`src/icc.rs`). Upstream starts one thread per core for every
  frame and builds a transform in each, about 2 ms apiece: a 256x256
  thumbnail cost 56 ms of processor time instead of under 3, and a transform
  that failed to build was reported as a successful conversion. The lcms2
  floor is 6.1.1, the oldest release checked for the shared transform, with
  the lcms2-sys release it needs. `icc::chunked_matches_whole` checks it.

`Cargo.toml.orig`, `Cargo.lock` and the registry's bookkeeping files were left
out.

Remove this directory and the `[patch.crates-io]` entry in the workspace
`Cargo.toml` when upstream lets a pool or a loader choose its processes'
priority, releases idle loaders within the retention time and lets a caller
choose the memory format by the frame's colors, confines a loader without
user namespaces, and converts a small frame's colors on one thread.
