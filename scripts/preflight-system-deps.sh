#!/usr/bin/env bash
# proposal:declare-system-dependencies
#
# Fail loudly at setup instead of mysteriously at build.
#
# Motivating incident: crates/membrane-discord depends on `opus`, whose
# audiopus_sys build script asks pkg-config for a system Opus and only builds
# from source when that lookup fails. The dev Macs have opus from Homebrew, so
# the source path never ran locally and the dependency was INVISIBLE — the
# workspace appeared to build from a clean checkout when it actually depended on
# machine state. The first CI run on a bare macOS runner took the source path
# and died: opus's bundled CMakeLists declares cmake_minimum_required(VERSION
# <3.5), which CMake 4 has removed support for. The dev Macs run CMake 4.3.3
# too, so a fresh Mac without Homebrew opus would have failed identically.
#
# Run: just preflight
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILURES=0
WARNINGS=0

pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
warn() { printf '  \033[33mWARN\033[0m %s\n' "$1"; WARNINGS=$((WARNINGS + 1)); }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }

need_command() {
    local cmd="$1" why="$2" fix="$3"
    if command -v "${cmd}" >/dev/null 2>&1; then
        pass "${cmd} — ${why}"
    else
        fail "${cmd} is missing — ${why}. Fix: ${fix}"
    fi
}

printf '\nPhilotic system dependency preflight\n\n'

printf 'Toolchain\n'
need_command cargo "builds the workspace" "https://rustup.rs"
need_command cc "libsqlite3-sys is used with the 'bundled' feature and compiles SQLite from source" \
    "xcode-select --install (macOS) / apt install build-essential (Debian)"

if command -v rustup >/dev/null 2>&1; then
    pass "rustup — rust-toolchain.toml pin is honoured"
else
    warn "rustup is not installed, so rust-toolchain.toml is INERT here and cargo/rustc come from elsewhere (e.g. Homebrew). CI pins 1.94.0; local builds will not."
fi

printf '\nAudio (crates/membrane-discord -> opus -> audiopus_sys)\n'
need_command pkg-config "audiopus_sys asks pkg-config for a system Opus before falling back to a source build" \
    "brew install pkg-config / apt install pkg-config"

if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists opus 2>/dev/null; then
    pass "opus $(pkg-config --modversion opus) found via pkg-config — audiopus_sys uses the system library, no CMake needed"
else
    # Not fatal: the source build CAN work. But only with a CMake that still
    # accepts opus's ancient cmake_minimum_required, which CMake 4 does not.
    if command -v cmake >/dev/null 2>&1; then
        cmake_major="$(cmake --version 2>/dev/null | head -1 | sed -E 's/[^0-9]*([0-9]+).*/\1/')"
        if [[ "${cmake_major:-0}" -ge 4 ]]; then
            fail "opus is not installed AND cmake is ${cmake_major}.x — audiopus_sys will fall back to a source build that CMake >= 4 rejects (cmake_minimum_required < 3.5). Fix: brew install opus (or export CMAKE_POLICY_VERSION_MINIMUM=3.5)"
        else
            warn "opus is not installed; audiopus_sys will build it from source with cmake ${cmake_major}.x. Slower, but should work. Fix: brew install opus"
        fi
    else
        fail "opus is not installed and cmake is missing — audiopus_sys can neither find nor build Opus. Fix: brew install opus"
    fi
fi

printf '\nMachine learning (crates/onnx-runner -> ort)\n'
if [[ -n "${ORT_LIB_LOCATION:-}" ]]; then
    pass "ORT_LIB_LOCATION is set — ort uses a local ONNX Runtime"
else
    warn "ort is configured with 'download-binaries', so the FIRST build downloads ONNX Runtime and needs network access. Set ORT_LIB_LOCATION to use a local copy."
fi

printf '\nDesktop UI (crates/philotic-web/build.rs)\n'
if [[ -n "${PHILOTIC_DESKTOP_DIR:-}" || -n "${PHILOTIC_REFRESH_DESKTOP_UI:-}" ]]; then
    need_command npm "PHILOTIC_DESKTOP_DIR / PHILOTIC_REFRESH_DESKTOP_UI is set, so build.rs will run 'npm install' and 'npm run build' — and it PANICS if npm is missing" \
        "brew install node"
else
    pass "PHILOTIC_DESKTOP_DIR unset — build.rs reuses the committed ui-dist and never invokes npm"
fi

printf '\n'
if [[ "${FAILURES}" -eq 0 ]]; then
    if [[ "${WARNINGS}" -gt 0 ]]; then
        printf 'Preflight passed with %d warning(s).\n\n' "${WARNINGS}"
    else
        printf 'Preflight passed.\n\n'
    fi
    exit 0
fi

printf 'Preflight failed: %d missing dependency/dependencies.\n\n' "${FAILURES}" >&2
exit 1
