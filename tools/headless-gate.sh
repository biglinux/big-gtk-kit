#!/bin/bash
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
#
# headless-gate.sh — run any command under a PRIVATE virtual KWin session (pure
# software GTK by default; KWin may still use the GPU). Tests run invisibly here
# instead of mapping windows on the developer's live desktop.
#
# KWin's virtual backend, not weston headless: weston headless exposes a seat
# with no capabilities, so GTK can never grab for a popover and nothing ever
# takes keyboard focus. Menu and focus tests then fail on correct widgets.
# Weston's --fake-seat is not a way out — it segfaults libweston 15 on the first
# popup grab.
#
# Usage: tools/headless-gate.sh [--accessible] <command> [args…]
#        tools/headless-gate.sh cargo test -- --include-ignored
#        tools/headless-gate.sh --accessible python3 tools/sweep/leak_sweep.py SURFACES.toml
# Exit: the command's exit code, 2 on missing deps.
#
# `--accessible` swaps the test AT context for the real AT-SPI bridge and starts a
# registry on the private bus, which is what an accessibility walk or a screen reader
# needs and what a unit test must not have: the two modes are mutually exclusive, and
# both were needed by the same session harness.
set -Eeuo pipefail

accessible=0
if [[ ${1:-} == --accessible ]]; then
	accessible=1
	shift
fi

if ! command -v kwin_wayland >/dev/null 2>&1; then
	echo "SKIP headless-gate: kwin_wayland is not installed." >&2
	exit 2
fi
if ! command -v dbus-daemon >/dev/null 2>&1; then
	echo "SKIP headless-gate: dbus-daemon is not installed." >&2
	exit 2
fi
if [[ $# -lt 1 ]] || ! command -v "$1" >/dev/null 2>&1; then
	echo "usage: headless-gate.sh [--accessible] <command> [args…]" >&2
	exit 2
fi
registry="/usr/lib/at-spi2-registryd"
if [[ "$accessible" == 1 && ! -x "$registry" ]]; then
	echo "SKIP headless-gate: $registry is not installed." >&2
	exit 2
fi

# The name is terse on purpose: this stands in for XDG_RUNTIME_DIR, and an SSH
# control socket under it must fit a 108-byte sun_path. `/run/user/1000` is 14
# bytes, and anything past 19 leaves an SSH client without connection reuse.
#
# `/dev/shm` rather than /tmp, and 8 bytes keeps the budget: a real
# XDG_RUNTIME_DIR allows execution, and a distribution that mounts /tmp `noexec`
# — BigLinux does — makes every test that runs a stub it wrote itself silently
# run the installed program instead. /dev/shm is the one short tmpfs that is
# always present and never noexec.
runtime_dir="$(mktemp -d /dev/shm/bg.XXXXXX)"
chmod 700 "$runtime_dir"
socket="big-headless-gate"

cleanup() {
	for pid in "${registry_pid:-}" "${a11y_pid:-}" "${kwin_pid:-}" "${bus_pid:-}"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	rm -rf "$runtime_dir"
}
trap cleanup EXIT

# Activation-restricted private session bus: the EMPTY servicedir means no
# desktop service (portals, gvfs, at-spi, kwallet) can be D-Bus-activated —
# the honest headless baseline, and no daemons leak past the run. The bus comes
# first because the compositor needs it.
mkdir -p "$runtime_dir/services"
cat >"$runtime_dir/session.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=$runtime_dir</listen>
  <servicedir>$runtime_dir/services</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
DBUS_SESSION_BUS_ADDRESS="$(dbus-daemon --fork --config-file="$runtime_dir/session.conf" \
	--print-address --print-pid=3 3>"$runtime_dir/bus.pid")"
export DBUS_SESSION_BUS_ADDRESS
bus_pid="$(<"$runtime_dir/bus.pid")"

# Only the interactive accessibility walks need input injection and compositor
# screenshots. Component tests render their own widgets and keep KWin's checks.
kwin_environment=()
if [[ "$accessible" == 1 ]]; then
	kwin_environment=(KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 KWIN_SCREENSHOT_NO_PERMISSION_CHECKS=1)
fi
env -u KWIN_WAYLAND_NO_PERMISSION_CHECKS -u KWIN_SCREENSHOT_NO_PERMISSION_CHECKS \
	XDG_RUNTIME_DIR="$runtime_dir" "${kwin_environment[@]}" \
	kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
	--width 1280 --height 800 --socket="$socket" \
	>/dev/null 2>&1 &
kwin_pid=$!

# Wait for the compositor socket (max ~10s).
for ((attempt = 0; attempt < 100; attempt++)); do
	[[ -S "$runtime_dir/$socket" ]] && break
	sleep 0.1
done
if [[ ! -S "$runtime_dir/$socket" ]]; then
	# 1, not 2. Exit 2 means "a tool this machine lacks", and the release gate
	# writes it to the ledger as SKIP. A compositor that was found and then failed
	# to come up is a failure: reporting it as "not applicable" turns every
	# display-dependent test in the run into a silent pass.
	echo "FAIL headless-gate: kwin_wayland socket never appeared" >&2
	exit 1
fi

# The accessibility bus, when asked for. `at-spi-bus-launcher` owns `org.a11y.Bus`
# and hands out the address every GTK application's bridge asks for; the registry is
# what turns a pile of applications into one tree a screen reader can walk. Both are
# started by hand because the private bus has an EMPTY servicedir on purpose — the
# baseline is "nothing is activatable", and an accessibility session says which two
# daemons it wants rather than opening the door to all of them.
if [[ "$accessible" == 1 ]]; then
	XDG_RUNTIME_DIR="$runtime_dir" /usr/lib/at-spi-bus-launcher --launch-immediately \
		>"$runtime_dir/at-spi-bus.log" 2>&1 &
	a11y_pid=$!
	# The launcher answers on the session bus before the registry can be started, and a
	# registry that starts first simply exits.
	for ((attempt = 0; attempt < 100; attempt++)); do
		if dbus-send --session --dest=org.a11y.Bus --print-reply=literal \
			/org/a11y/bus org.a11y.Bus.GetAddress >/dev/null 2>&1; then
			break
		fi
		sleep 0.1
	done
	XDG_RUNTIME_DIR="$runtime_dir" "$registry" \
		>"$runtime_dir/at-spi-registry.log" 2>&1 &
	registry_pid=$!
fi

# Software GSK renderer. GTK's own test AT context replaces the AT-SPI bridge,
# which injects spurious teardown noise when headless: `GTK_A11Y=none` would
# also stop recording accessible properties, so `gtk_test_accessible_*`
# assertions would fail on correct widgets. DISPLAY is cleared so nothing falls
# back to X11. LC_NUMERIC=C because `mpv_create` returns NULL under a
# comma-decimal locale, and a test binary has no startup code to set it.
# LC_MESSAGES=C (and an empty LANGUAGE, which gettext prefers) keeps assertions
# on user-visible strings comparing against the msgid the source declares
# instead of whatever translation the developer's session happens to install.
# GSK_RENDERER defaults to cairo -- software, reproducible, no GPU needed on a
# build machine -- but an explicit one from the caller wins, because measuring
# what a renderer costs on the GPU is impossible against a renderer that never
# touches it.
if [[ "$accessible" == 1 ]]; then
	# **A home of its own, because this mode runs the PRODUCT and not a test binary.**
	# Without it a desktop shell read and wrote the developer's real configuration and
	# notification history: a walk here showed a banner from a probe two runs earlier,
	# kept under `~/.local/state`.
	# A session that edits the state of the person running it cannot be run twice and
	# mean the same thing.
	home="$runtime_dir/home"
	mkdir -p "$home"/{.config,.local/share,.local/state,.cache}

	# The real bridge, and the language the product ships in rather than the msgids:
	# an accessibility walk is about what a person hears, and a person hears their own
	# language. `BIG_A11Y_SESSION` is how the walk finds the session's own directory
	# for the logs it wants to keep.
	XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" DISPLAY='' \
		HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
		XDG_STATE_HOME="$home/.local/state" XDG_CACHE_HOME="$home/.cache" \
		GDK_BACKEND=wayland GSK_RENDERER="${GSK_RENDERER:-cairo}" GTK_A11Y=atspi \
		LC_NUMERIC=C BIG_A11Y_SESSION="$runtime_dir" \
		"$@"
else
	XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" DISPLAY='' \
		GDK_BACKEND=wayland GSK_RENDERER="${GSK_RENDERER:-cairo}" GTK_A11Y=test NO_AT_BRIDGE=1 \
		LC_NUMERIC=C LC_MESSAGES=C LANGUAGE='' \
		"$@"
fi
