#!/bin/bash
# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
#
# headless-gate.sh: run any command under a private virtual KWin session (pure
# software GTK by default; KWin may still use the GPU). Tests run invisibly here
# instead of mapping windows on the developer's live desktop.
#
# KWin's virtual backend, not weston headless: weston headless exposes a seat
# with no capabilities, so GTK can never grab for a popover and nothing ever
# takes keyboard focus. Menu and focus tests then fail on correct widgets.
# Weston's --fake-seat is no way out: it segfaults libweston 15 on the first
# popup grab.
#
# Usage: tools/headless-gate.sh [--accessible] <command> [args…]
#        tools/headless-gate.sh cargo test -- --include-ignored
#        tools/headless-gate.sh --accessible python3 tools/sweep/leak_sweep.py SURFACES.toml
# Exit: the command's exit code, 2 on missing deps.
#
# `--accessible` swaps the test AT context for the real AT-SPI bridge and starts a
# registry on the private bus, which is what an accessibility walk or a screen reader
# needs and what a unit test must not have, so one run is either one or the other.
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

# The name is terse because it stands in for XDG_RUNTIME_DIR, and an SSH
# control socket under it must fit a 108-byte sun_path. `/run/user/1000` is 14
# bytes, and anything past 19 leaves an SSH client without connection reuse.
#
# `/dev/shm` rather than /tmp, and 8 bytes keeps the budget: a real
# XDG_RUNTIME_DIR allows execution, and a distribution that mounts /tmp `noexec`
# (BigLinux does) makes every test that runs a stub it wrote itself silently
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
	rm -rf "$runtime_dir" ${home:+"$home"}
}
trap cleanup EXIT

# What gui-session sets too: the census harness refuses to open windows without it.
export BIGLINUX_UI_SESSION_MODE=headless

# The private bus activates no dconf service, so GSettings keep their values in
# memory for the run and never reach the person's database.
export GSETTINGS_BACKEND=memory

# A caller inside a systemd service (a terminal in the person's desktop) passes
# down that service's memory.pressure: the run's programs would write their
# trigger into the person's cgroup and wake when the person's desktop is short.
unset MEMORY_PRESSURE_WATCH MEMORY_PRESSURE_WRITE

# A home of its own: tests write desktop entries and settings into the data and
# config homes, and a program run here reads and writes its configuration and
# state. The tool homes keep the developer's paths, resolved before the switch:
# the command is usually cargo, and a compiler cache server started from this
# home would outlive the run pointing into it. It sits beside TMPDIR, not under
# the runtime dir: tests move files from TMPDIR into the home's Trash, which
# needs both on one filesystem.
home="$(mktemp -d "${TMPDIR:-/tmp}/bg-home.XXXXXX")"
mkdir -p "$home"/{.config,.local/share,.local/state,.cache}
home_environment=(
	HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share"
	XDG_STATE_HOME="$home/.local/state" XDG_CACHE_HOME="$home/.cache"
	CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
	SCCACHE_CONF="${SCCACHE_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/sccache/config}"
	SCCACHE_DIR="${SCCACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/sccache}"
)
# A new home has no GStreamer registry, so every process that loads GStreamer
# would scan the plugins itself, and tests doing it at once outrun their time
# limits. One scan here leaves the registry in the home for all of them.
if command -v gst-inspect-1.0 >/dev/null; then
	env "${home_environment[@]}" gst-inspect-1.0 >/dev/null 2>&1 || true
fi

# Activation-restricted private session bus: the empty servicedir means no
# desktop service (portals, gvfs, at-spi, kwallet) can be D-Bus-activated:
# the headless baseline, and no daemons leak past the run. The bus comes
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
	XDG_RUNTIME_DIR="$runtime_dir" "${home_environment[@]}" "${kwin_environment[@]}" \
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
	# 1, not 2. Exit 2 means "a tool this machine lacks", which a gate reads
	# as a skipped step. A compositor that was found and then failed
	# to come up is a failure: reporting it as "not applicable" turns every
	# display-dependent test in the run into a silent pass.
	echo "FAIL headless-gate: kwin_wayland socket never appeared" >&2
	exit 1
fi

# The accessibility bus, when asked for. `at-spi-bus-launcher` owns `org.a11y.Bus`
# and hands out the address every GTK application's bridge asks for; the registry is
# what turns a pile of applications into one tree a screen reader can walk. Both are
# started by hand because the private bus has an empty servicedir: the
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
# GSK_RENDERER defaults to cairo (software, reproducible, no GPU needed on a
# build machine), but an explicit one from the caller wins, because measuring
# what a renderer costs on the GPU is impossible against a renderer that never
# touches it. BIG_RUN_UI_SMOKE lets big-testkit's harness open windows: without
# it every widget test passes without running.
if [[ "$accessible" == 1 ]]; then
	# The real bridge, and the language the program ships in rather than the msgids:
	# an accessibility walk is about what a person hears, and a person hears their own
	# language.
	env XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" DISPLAY='' "${home_environment[@]}" \
		GDK_BACKEND=wayland GSK_RENDERER="${GSK_RENDERER:-cairo}" GTK_A11Y=atspi \
		BIG_RUN_UI_SMOKE=1 LC_NUMERIC=C \
		"$@"
else
	# The simple input method: GTK 4.22's Wayland one crashes in text_input's
	# `enter` when the first text field a process focused is destroyed before
	# the main loop reads the compositor's answer, which tests do.
	env XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" DISPLAY='' "${home_environment[@]}" \
		GDK_BACKEND=wayland GSK_RENDERER="${GSK_RENDERER:-cairo}" GTK_A11Y=test NO_AT_BRIDGE=1 \
		GTK_IM_MODULE=gtk-im-context-simple \
		BIG_RUN_UI_SMOKE=1 LC_NUMERIC=C LC_MESSAGES=C LANGUAGE='' \
		"$@"
fi
