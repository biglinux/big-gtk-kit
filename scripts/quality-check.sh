#!/bin/bash
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
#
# Every check a change to this repository passes; exits with the first failure.
set -euo pipefail
cd "$(dirname "$0")/.."

# Tests that read the AT-SPI bridge, which only --accessible starts.
bridge='closed_about_keeps_cache|removed_link_label_keeps_cache|first_refusal_is_announced|refusal_before_window_is_announced|work_start_is_announced|capture_refusal_is_announced|named_drop_down_heard_with_choice'

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked --config 'build.warnings="deny"'
tools/headless-gate.sh cargo nextest run --workspace --all-features --locked
# The patched glycin copy is outside the workspace, which --workspace skips.
cargo nextest run -p glycin --lib --locked
tools/headless-gate.sh cargo nextest run --workspace --all-features --locked --run-ignored only -j 1 \
	-E "not test(/::(mount_change_is_seen|$bridge)\$/)"
tools/headless-gate.sh --accessible cargo nextest run --workspace --all-features --locked \
	--run-ignored only -E "test(/::($bridge)\$/)"
# Mounts in a mount namespace of its own, which goes away with the test.
cargo test -p big-os-kit --lib --locked \
	--config 'target.x86_64-unknown-linux-gnu.runner="unshare -rm"' -- --ignored mount_change_is_seen
tools/headless-gate.sh cargo test --workspace --all-features --locked --doc
RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps --locked
ast-grep scan
ast-grep test --skip-snapshot-tests
cargo deny --workspace check
ruff check .
ruff format --check .
make -C tools check
