# Philotic Stack Management - Command Center Compliant
# Run `just` to see available commands

default:
    @just --list

# Build the entire Philotic Stack workspace
build:
    cargo build --workspace

# Check the entire Philotic Stack workspace without building artifacts
check:
    cargo check --workspace

# Verify the repo bootstrap engine: Muninn, helper scripts, and workspace baseline.
engine-check:
    ./scripts/engine-check.sh

# Install repo-local git hooks such as the deterministic pre-push secret check.
install-git-hooks:
    git config core.hooksPath .githooks

# Mandatory Muninn bootstrap gate for meaningful sessions.
session-start:
    python3 scripts/muninn_mcp.py bootstrap
    just harness-drift 2>/dev/null || true
    bash scripts/session-start.sh
    ./scripts/idea-sweep.sh pending || echo "⚠ idea sweep skipped (Memgraph unreachable) — run 'just idea-sweep' later"

# Aria idea pipeline (stage 2): sweep + triage operator ideas in the LifeGraph.
# Verbs: pending (default) | all | promote <idea:slug> <graph-ref> | decline <idea:slug> <reason> | ship <idea:slug> [note]
idea-sweep *args="pending":
    ./scripts/idea-sweep.sh {{args}}

# Verify private native Muninn access, including the vps-jane SSH tunnel path.
muninn-private-smoke:
    ./scripts/muninn-private-access.sh smoke

# Verify local/private MCP client credential posture without printing secrets.
mcp-client-uat mode="safe":
    ./scripts/mcp-client-uat.sh {{mode}}

# Non-mutating Muninn cluster lab preflight. Does not enable cluster mode.
muninn-cluster-preflight mode="local":
    ./scripts/muninn-cluster-lab-preflight.sh {{mode}}

# Show drift status for all managed harnesses.
harness-drift:
    @phil graph harness drift

# Verify every managed harness against its projection, then report drift.
harness-verify-all:
    #!/usr/bin/env bash
    set -uo pipefail
    phil graph harness list | awk 'NR>2 {print $1}' | while read -r h; do
        [ -n "$h" ] && phil graph harness verify "$h" || true
    done
    phil graph harness drift

# Sync the harness skill catalog in the graph from skills/*/SKILL.md.
harness-skills-sync:
    phil graph harness skills sync

# Install the launchd schedule that refreshes graph scan + harness drift every 6 hours.
intel-graph-freshness-schedule:
    ./scripts/install-intel-graph-freshness-schedule.sh

# Install the launchd service that SUPERVISES the graph server (KeepAlive, RunAtLoad).
intel-graph-service:
    ./scripts/install-intel-graph-service.sh

# Re-apply the canonical profile to a harness (default: claude-local with philotic-operator).
harness-apply harness="claude-local" profile="philotic-operator":
    phil graph harness apply {{harness}} --profile {{profile}}
    phil graph harness verify {{harness}}

# Start a measured harness trial for focused work on a seam.
# Usage: just harness-trial-start <seam-id> [harness] [profile]
harness-trial-start seam harness="claude-local" profile="philotic-operator":
    #!/usr/bin/env bash
    SESSION=$(phil graph harness trials start {{harness}} {{seam}} \
      --profile {{profile}} --agent claude --agent-model claude-sonnet-4-6 2>&1 | grep -oE 'session:[^ ]+' | head -1)
    echo "Trial started: $SESSION"
    echo "$SESSION" > /tmp/philotic-harness-trial-session

# Report activity against the current harness trial.
# Usage: just harness-trial-report <activity-type> [phase] [tokens_in] [tokens_out] [elapsed_ms] [lines_changed] [files] [note]
harness-trial-report activity phase="" tokens_in="0" tokens_out="0" elapsed_ms="0" lines_changed="0" files="" note="":
    #!/usr/bin/env bash
    set -euo pipefail
    SESSION=$(cat /tmp/philotic-harness-trial-session 2>/dev/null || echo "")
    if [ -z "$SESSION" ]; then echo "No active trial session (run harness-trial-start first)"; exit 1; fi
    ARGS=("$SESSION" "{{activity}}")
    HAS_SIGNAL=0
    if [ -n "{{phase}}" ]; then ARGS+=(--phase "{{phase}}"); HAS_SIGNAL=1; fi
    if [ "{{tokens_in}}" != "0" ]; then ARGS+=(--tokens-input "{{tokens_in}}"); HAS_SIGNAL=1; fi
    if [ "{{tokens_out}}" != "0" ]; then ARGS+=(--tokens-output "{{tokens_out}}"); HAS_SIGNAL=1; fi
    if [ "{{elapsed_ms}}" != "0" ]; then ARGS+=(--elapsed-ms "{{elapsed_ms}}"); HAS_SIGNAL=1; fi
    if [ "{{lines_changed}}" != "0" ]; then ARGS+=(--lines-changed "{{lines_changed}}"); HAS_SIGNAL=1; fi
    if [ -n "{{files}}" ]; then ARGS+=(--files "{{files}}"); HAS_SIGNAL=1; fi
    if [ -n "{{note}}" ]; then ARGS+=(--note "{{note}}"); HAS_SIGNAL=1; fi
    if [ "$HAS_SIGNAL" -eq 0 ]; then
        echo "No telemetry supplied; report at least one signal (phase, tokens, elapsed_ms, lines_changed, files, or note)"
        exit 1
    fi
    phil graph harness trials report "${ARGS[@]}"

# Close the current harness trial.
# Usage: just harness-trial-close [status] [verified] [summary]
harness-trial-close status="completed" verified="" summary="":
    #!/usr/bin/env bash
    set -euo pipefail
    SESSION=$(cat /tmp/philotic-harness-trial-session 2>/dev/null || echo "")
    if [ -z "$SESSION" ]; then echo "No active trial session found"; exit 1; fi
    if [ "{{status}}" = "completed" ] && [ -z "{{verified}}" ]; then
        echo "Completed harness trials must include a verification level (for example, test-green)"
        exit 1
    fi
    ARGS=("$SESSION" --status "{{status}}")
    if [ -n "{{verified}}" ]; then ARGS+=(--verified "{{verified}}"); fi
    if [ -n "{{summary}}" ]; then ARGS+=(--summary "{{summary}}"); fi
    phil graph harness trials close "${ARGS[@]}"
    rm -f /tmp/philotic-harness-trial-session
    echo "Trial closed: $SESSION"

# Merge mesh-config.secrets.json into mesh-config.json.
# mesh-config.secrets.json holds only rotating secrets (API keys, bot tokens).
# mesh-config.example.json holds the full structure with placeholders — commit that, not the secrets.
sync-config:
    #!/usr/bin/env python3
    import json, sys, os
    base = json.load(open("mesh-config.json")) if os.path.exists("mesh-config.json") else json.load(open("mesh-config.example.json"))
    if not os.path.exists("mesh-config.secrets.json"):
        print("mesh-config.secrets.json not found — nothing to merge"); sys.exit(0)
    secrets = json.load(open("mesh-config.secrets.json"))
    def deep_merge(a, b):
        for k, v in b.items():
            if k in a and isinstance(a[k], dict) and isinstance(v, dict):
                deep_merge(a[k], v)
            else:
                a[k] = v
    deep_merge(base, secrets)
    json.dump(base, open("mesh-config.json", "w"), indent=2)
    print("mesh-config.json updated from secrets")

# Start the Hotel Manager (Aiua Host Daemon)
start-aiua hotel:
    cargo build --workspace
    cargo run -p aiua -- --hotel {{hotel}}

# Rebuild the local runtime binaries that the hotel materializes during watched UAT.
build-runtime:
    cargo build -p aiua -p philote -p membrane-telegram -p model-router -p tool-runner -p graph-datasource -p philotic-web

# Kill local Philotic hotel/guest binaries from this checkout and clear stale sockets.
kill-local-stack:
    @pkill -KILL -f "target/debug/aiua" 2>/dev/null || true
    @pkill -KILL -f "target/debug/membrane" 2>/dev/null || true
    @pkill -KILL -f "target/debug/philote" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-gemini" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-elevenlabs" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-openrouter" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-anthropic" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-openai" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-ollama" 2>/dev/null || true
    @pkill -KILL -f "target/debug/tool-runner" 2>/dev/null || true
    @pkill -KILL -f "target/debug/graph-runner" 2>/dev/null || true
    @pkill -KILL -f "target/debug/graph-datasource" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-mlx" 2>/dev/null || true
    @sleep 0.3
    @rm -f /tmp/philotic-*.sock

# Nuclear clear: kill ALL aiua and guest processes (both debug and Homebrew installs),
# remove all sockets (tmp and profile dirs). Safe to run anytime — use when FD exhaustion
# or stale processes are suspected, or before a clean restart of any hotel.
clear-aiua:
    @echo "Clearing all aiua processes and sockets…"
    @pkill -KILL -f "aiua" 2>/dev/null || true
    @pkill -KILL -f "membrane" 2>/dev/null || true
    @pkill -KILL -f "philote" 2>/dev/null || true
    @pkill -KILL -f "model-router" 2>/dev/null || true
    @pkill -KILL -f "tool-runner" 2>/dev/null || true
    @pkill -KILL -f "graph-runner" 2>/dev/null || true
    @pkill -KILL -f "agent-datasource" 2>/dev/null || true
    @sleep 0.5
    @rm -f /tmp/philotic-*.sock
    @find "${HOME}/.philotic" -name "*.sock" -delete 2>/dev/null || true
    @echo "Done. All aiua processes and sockets cleared."

# Rebuild first, then kill stale local runtime processes/sockets, then start one hotel cleanly.
start-aiua-clean hotel:
    just build-runtime
    just kill-local-stack
    cargo run -p aiua -- --hotel {{hotel}}

# Wait for the hotel socket then start philotic-web serve.
# Usage: just start-serve local-telegram   (run in a second terminal after start-aiua-clean)
start-serve hotel:
    #!/usr/bin/env bash
    set -euo pipefail
    SOCK="/tmp/philotic-{{hotel}}.sock"
    echo "Waiting for ${SOCK}..."
    for i in $(seq 1 60); do
      [[ -S "${SOCK}" ]] && break
      [[ ${i} -eq 60 ]] && echo "Timed out waiting for ${SOCK}" && exit 1
      sleep 0.5
    done
    echo "Socket ready — starting serve"
    exec "$(pwd)/target/debug/philotic-web" serve

# Start the transitional Gemini OAuth flow through the hotel CLI
gemini-oauth-start client_id project_id:
    @echo "Using GOOGLE_CLIENT_SECRET from env if needed by the OAuth client."
    @echo "On macOS, the hotel will use or create a Keychain-backed vault root key automatically."
    @echo "PHILOTIC_VAULT_MASTER_KEY remains a fallback for non-macOS or explicit override cases."
    cargo run -p aiua -- auth google start --provider gemini --client-id {{client_id}} --project-id {{project_id}}

# Validate that stored Gemini OAuth auth can call a real Gemini model
gemini-oauth-validate:
    cargo run -p aiua -- auth google validate --provider gemini

# Start the local UAT stack. Aiua will materialize the gateway, agent, model, and tool guests.
# Pass worktree=<path> to run a worktree's philote binary instead of the main one.
# Example: just uat worktree=../philotic-stack-philote-role-handoff
uat worktree="":
    #!/usr/bin/env bash
    set -euo pipefail
    just kill-local-stack
    if [ -n "{{worktree}}" ]; then
        abs="$(cd "{{worktree}}" && pwd)"
        export PHILOTIC_BIN_DIR="$abs/target/debug"
        echo "UAT: using worktree binary from $PHILOTIC_BIN_DIR"
        cargo build --manifest-path "{{worktree}}/Cargo.toml" -p philote
    fi
    echo "Starting UAT stack on hotel 'local-telegram'..."
    echo "Only one Telegram poller should be running for this bot token."
    cargo run -p aiua -- --hotel local-telegram

# (legacy alias)
start-aiua-uat:
    just uat

# Start the Gateway (Telegram Membrane)
start-gateway:
    cargo run -p membrane-telegram

# Start the Persona (Philote)
start-agent:
    cargo run -p philote

# Start the Mind (Model Router)
start-model:
    cargo run -p model-router

# Start the Parakeet ASR controller
start-parakeet:
    cargo run --bin model-controller-parakeet

# Start the Tool Runner
start-tool:
    cargo run -p tool-runner

# Start the Graph Datasource (Cypher-over-SQLite graph store guest)
start-graph-datasource:
    cargo run -p graph-datasource

# Start the full stack in background (requires tmux or similar)
start:
    @echo "Starting the Philotic Stack..."
    @echo "To run these properly, you should run the individual start-* commands in separate panes."

# Check the status of the local mesh (pings the Aiua host daemon)
status:
    @echo "Checking Philotic Stack local status..."
    @# Ping the Aiua daemon port or check processes.
    @ps aux | grep -v grep | grep "cargo run -p aiua" || echo "Aiua daemon is not running."
    @ps aux | grep -v grep | grep "cargo run -p membrane-telegram" || echo "Membrane gateway is not running."

# Build and install phil symlink to /usr/local/bin (dev workflow shortcut)
phil-install:
    cargo build -p philotic-web
    @ln -sf "$(pwd)/target/debug/philotic-web" /usr/local/bin/phil
    @echo "phil installed → /usr/local/bin/phil"

# Run philotic-web CLI (dev shortcut)
phil *args:
    cargo run -p philotic-web -- {{args}}

# Format code
format:
    cargo fmt --all

# Create a dedicated Codex worktree for an active thread.
worktree-create slug base="main":
    ./scripts/codex-worktree.sh create {{slug}} {{base}}

# Bootstrap an implementation workstream with a dedicated sibling worktree and checklist.
workstream-start slug base="develop":
    ./scripts/codex-workstream.sh start {{slug}} {{base}}
    @echo "Tip: record slice telemetry — just harness-trial-start <seam-id> (close at slice end with harness-trial-close)"

# Alias for the multi-role workstream workflow.
start-workstream slug base="develop":
    ./scripts/codex-workstream.sh start {{slug}} {{base}}

# Show git status plus hot-file overlap for an active workstream.
workstream-status slug compare_ref="origin/develop":
    ./scripts/codex-workstream.sh status {{slug}} {{compare_ref}}

# Show only hot-file overlap for an active workstream.
workstream-overlap slug compare_ref="origin/develop":
    ./scripts/codex-workstream.sh overlap {{slug}} {{compare_ref}}

# List registered git worktrees for this repo.
worktree-list:
    ./scripts/codex-worktree.sh list

# Print the sibling-path convention for a worktree slug.
worktree-path slug:
    ./scripts/codex-worktree.sh path {{slug}}

# Remove a dedicated Codex worktree when the thread is done.
worktree-remove slug delete_branch="":
    ./scripts/codex-worktree.sh remove {{slug}} {{delete_branch}}

# Prune stale git worktree metadata.
worktree-prune:
    ./scripts/codex-worktree.sh prune

# Report which sibling worktrees are safe to garbage-collect (dry run — deletes nothing).
worktree-gc:
    ./scripts/worktree-gc.sh --dry-run

# Garbage-collect merged+clean sibling worktrees to reclaim cargo target/ disk (real deletion).
worktree-gc-apply:
    ./scripts/worktree-gc.sh --apply

# Install the launchd schedule that runs worktree-gc --apply every 2 hours (mac-jane / macOS).
worktree-gc-schedule:
    ./scripts/install-worktree-gc-schedule.sh

# Run tests, then record pass/fail totals to the intel graph by default
# (graceful no-op notice if the graph server at :8900 isn't running).
test:
    ./scripts/test-and-record.sh

# Build the Apple edge client (PhiloticKit + PhiloticApp for macOS and iOS Simulator).
app-build:
    ./scripts/apple-app-build.sh

# Test the Apple edge client (PhiloticKit swift test, then both PhiloticApp builds).
app-test:
    ./scripts/apple-app-test.sh

# Run the heavier binary-level smoke test
smoke-binaries:
    ./scripts/smoke-binary-roundtrip.sh

# Run the routed tool binary smoke test
smoke-routed-tool:
    ./scripts/smoke-routed-tool-roundtrip.sh

# Run the approval interrupt binary smoke test
smoke-approval:
    ./scripts/smoke-approval-roundtrip.sh

# Run the approval denial binary smoke test
smoke-deny:
    ./scripts/smoke-deny-roundtrip.sh

# Run the approval steering binary smoke test
smoke-approve-steer:
    ./scripts/smoke-approve-steer-roundtrip.sh

# Run the deny redirect binary smoke test
smoke-deny-redirect:
    ./scripts/smoke-deny-redirect-roundtrip.sh

# Run the preapproved session binary smoke test
smoke-preapprove:
    ./scripts/smoke-preapprove-roundtrip.sh

# Run the subagent spawn/lease/assign/hook smoke test
smoke-subagent:
    ./scripts/smoke-subagent-roundtrip.sh

# Run the session lifecycle/control binary smoke test
smoke-session-control:
    ./scripts/smoke-session-control-roundtrip.sh

# Run the MCP client UAT (safe modes only; no live tokens required)
smoke-mcp:
    ./scripts/mcp-client-uat.sh safe

# Run the session bindings binary smoke test
smoke-session-bindings:
    ./scripts/smoke-session-bindings-roundtrip.sh

# Run the structured cognitive startup smoke (fake Gemini — no live credentials needed)
smoke-cognitive:
    bash scripts/smoke-cognitive-roundtrip.sh

# Run the cognitive re-entry smoke (session resume + turn loop continuation)
smoke-cognitive-reentry:
    bash scripts/smoke-cognitive-reentry-roundtrip.sh

# Run the ONNX embedding sidecar smoke (downloads model on first run, ~300 MB)
smoke-embed:
    bash scripts/smoke-embed-roundtrip.sh

# Run the graph-datasource smoke (create partition → CREATE node → MATCH → list round-trip)
smoke-graph-datasource:
    bash scripts/smoke-graph-datasource-roundtrip.sh

# Run the LifeGraph runner through live hotel IPC.
# Set PHILOTIC_HOTEL_SOCKET, PHILOTIC_TARGET_NODE, and PHILOTIC_REPLY_NODE for remote hotels.
smoke-life-graph-ipc:
    cargo run -p philotic-client --example life_graph_ipc_smoke_driver

# Run the agent-graph-runner live smoke (write + declare + sync round-trip, Seams 3 & 4)
smoke-agent-graph:
    bash scripts/smoke-agent-graph-roundtrip.sh

# Run the desktop membrane management surface smoke (lease, REST API, auth, clean shutdown)
smoke-desktop-membrane:
    bash scripts/smoke-desktop-membrane.sh

# Run the data-driven tool-grants roundtrip smoke: disable a tool at runtime with
# no rebuild, prove it leaves the composed session snapshot, and prove the boot
# seeder does not revert it across a hotel restart (ephemeral throwaway hotel).
smoke-tool-grants:
    bash scripts/smoke-tool-grants-roundtrip.sh

# Run the `phil config get/set` IPC roundtrip smoke (ephemeral throwaway hotel)
smoke-config:
    bash scripts/smoke-config-roundtrip.sh

# Substrate Hardening S4: run ONE bounded chaos-smoke scenario against a
# designated hotel (guest-kill / config-corrupt / mesh-peer-drop, or omit to
# round-robin the two real scenarios). Pass --dry-run to print the plan only.
# See scripts/chaos-smoke.sh's header for the full env-var contract —
# PHILOTIC_CHAOS_HOTEL / PHILOTIC_CHAOS_PROFILE / PHILOTIC_CHAOS_GUEST_ID in
# particular must be set to match the real target hotel before a live run.
chaos-smoke *args:
    bash scripts/chaos-smoke.sh {{args}}

# Install the OPT-IN weekly launchd schedule for chaos-smoke (macOS; never
# auto-installed — see scripts/install-chaos-smoke-schedule.sh).
chaos-smoke-schedule:
    ./scripts/install-chaos-smoke-schedule.sh

# Unit-test chaos-smoke.sh's assertion/parsing logic (denylists, JSON field
# extraction, heal-queue counting) against fixture data — no real hotel touched.
chaos-smoke-unit-test:
    bash scripts/tests/chaos-smoke-unit-test.sh

# Run the model-controller roundtrip smoke (requires mesh-config.json with model credentials)
smoke-model-controller:
    bash scripts/smoke-model-controller-roundtrip.sh

# Run the auto-recall smoke (session memory persistence + retrieval)
smoke-auto-recall:
    bash scripts/smoke-auto-recall.sh

# Run the remote-model roundtrip smoke (requires two running hotels + model credentials)
smoke-remote-model:
    bash scripts/smoke-remote-model-roundtrip.sh

# Run the Gemini OAuth roundtrip smoke (requires live Gemini OAuth credentials)
smoke-gemini-oauth:
    bash scripts/smoke-gemini-oauth-roundtrip.sh

# Run the Gemini Live complete-turn smoke (fake local Live websocket; no external creds)
smoke-gemini-live:
    bash scripts/smoke-gemini-live-roundtrip.sh

# Run the MLX model controller smoke (requires mlx_lm installed + Apple Silicon)
smoke-mlx:
    bash scripts/smoke-mlx-controller.sh

# Run the agent-datasource cargo integration tests (tool dispatch without live hotel, Seams 3 & 4)
test-agent-graph:
    cargo test -p agent-datasource --test smoke -- --nocapture

# Run the router trace cargo tests (RouterTraceStorage unit coverage, Seam 5)
test-router-trace:
    cargo test -p ansible-mesh-core -- router_trace --nocapture

# ── UAT Suites ────────────────────────────────────────────────────────────────

# Core unit + integration test suite (no binaries, fast)
test-suite:
    cargo test -p philotic-client -- --nocapture
    cargo test -p philote -- --nocapture
    cargo test -p aiua -- --nocapture
    cargo test -p agent-datasource --test smoke -- --nocapture
    cargo test -p ansible-mesh-core -- router_trace --nocapture

# Full binary smoke suite (no external credentials or large model downloads)
# Covers: routing, approval flows, session lifecycle, cognitive loop, graph runner,
#         agent graph, subagent, cognitive re-entry, desktop membrane.
smoke-suite:
    ./scripts/smoke-routed-tool-roundtrip.sh
    ./scripts/smoke-approval-roundtrip.sh
    ./scripts/smoke-deny-roundtrip.sh
    ./scripts/smoke-approve-steer-roundtrip.sh
    ./scripts/smoke-deny-redirect-roundtrip.sh
    ./scripts/smoke-preapprove-roundtrip.sh
    ./scripts/smoke-session-control-roundtrip.sh
    ./scripts/smoke-session-bindings-roundtrip.sh
    ./scripts/smoke-subagent-roundtrip.sh
    bash scripts/smoke-cognitive-roundtrip.sh
    bash scripts/smoke-cognitive-reentry-roundtrip.sh
    bash scripts/smoke-gemini-live-roundtrip.sh
    bash scripts/smoke-graph-datasource-roundtrip.sh
    bash scripts/smoke-agent-graph-roundtrip.sh
    bash scripts/smoke-desktop-membrane.sh

# Run the trusted vertical-slice verification suite (test-suite + smoke-suite)
verify-vertical-slice:
    just test-suite
    just smoke-suite

# Print the operator checklist for the current trusted vertical slice
operator-checklist:
    @echo "Philotic Stack UAT Checklist"
    @echo ""
    @echo "── Tier 1: No external deps (run always) ──────────────────────"
    @echo "  just verify-vertical-slice"
    @echo "    = just test-suite  (unit + integration tests)"
    @echo "    + just smoke-suite (binary roundtrip smokes, incl. desktop membrane)"
    @echo "  just smoke-gemini-live        # fake local Gemini Live complete-turn continuity"
    @echo ""
    @echo "── Tier 2: External credentials required ───────────────────────"
    @echo "  just smoke-model-controller   # mesh-config.json + Gemini key"
    @echo "  just smoke-auto-recall        # running hotel + Muninn (PHILOTIC_HOTEL_SOCKET must be set)"
    @echo "  just smoke-gemini-oauth       # live Gemini OAuth flow"
    @echo "  just smoke-remote-model       # two live hotels"
    @echo ""
    @echo "── Tier 3: Hardware / large download ───────────────────────────"
    @echo "  just smoke-embed              # ~300 MB ONNX model download"
    @echo "  just smoke-mlx                # Apple Silicon + mlx_lm installed"
    @echo ""
    @echo "── Watched-live UAT ────────────────────────────────────────────"
    @echo "  just uat                      # start local-telegram hotel"
    @echo "  Verify: guests alive, registered, routed tool flow succeeds"
    @echo ""
    @echo "── Confidence levels ───────────────────────────────────────────"
    @echo "  test-green        (just test-suite passes)"
    @echo "  smoke-green       (just smoke-suite passes)"
    @echo "  uat-green         (verify-vertical-slice + tier-2 passes)"
    @echo "  watched-live-green (live hotel + telegram confirms end-to-end)"

# Build release binaries and install them into the local Homebrew Cellar.
# Use this when jane is offline or to update the local phil/aiua without touching jane.
local-push:
    #!/usr/bin/env bash
    set -euo pipefail
    AIUA_CELLAR=/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin
    PHIL_CELLAR=/opt/homebrew/Cellar/philotic-web/0.1.0-alpha/bin
    AIUA_BINS="aiua philote membrane-telegram membrane-discord membrane-mcp model-router model-controller-gemini model-controller-elevenlabs model-controller-openrouter model-controller-anthropic model-controller-openai model-controller-mlx model-controller-ollama model-controller-onnx model-controller-parakeet model-controller-vision philote-worker tool-runner graph-datasource table-datasource router-listener agent-datasource heal-dispatcher life-graph-runner"
    echo "▶ Building release binaries..."
    cargo build --release -p aiua -p philote -p membrane-telegram -p membrane-discord -p membrane-mcp -p model-router -p tool-runner -p graph-datasource -p philotic-web -p table-datasource -p router-listener -p agent-datasource -p heal-dispatcher -p data-memorygraphrag
    echo "▶ Installing aiua stack to ${AIUA_CELLAR}..."
    # Make bin dir writable so we can delete+recreate files (new inode avoids macOS codesign cache poisoning)
    chmod u+w "${AIUA_CELLAR}"
    for bin in $AIUA_BINS; do
        if [ ! -f "target/release/$bin" ]; then
            echo "  – $bin (not built, skipping)"
            continue
        fi
        if [ ! -f "${AIUA_CELLAR}/$bin" ]; then
            cp "target/release/$bin" "${AIUA_CELLAR}/$bin"
            chmod 555 "${AIUA_CELLAR}/$bin"
            # Create /opt/homebrew/bin symlink for new binaries
            if [ ! -e "/opt/homebrew/bin/$bin" ]; then
                ln -s "../Cellar/aiua/0.1.0-alpha/bin/$bin" "/opt/homebrew/bin/$bin"
                echo "  + $bin (new + symlinked)"
            else
                echo "  + $bin (new)"
            fi
            continue
        fi
        rm -f "${AIUA_CELLAR}/$bin"
        cp "target/release/$bin" "${AIUA_CELLAR}/$bin"
        chmod 555 "${AIUA_CELLAR}/$bin"
        echo "  ✓ $bin"
    done
    chmod u-w "${AIUA_CELLAR}"
    echo "▶ Installing phil to ${PHIL_CELLAR}..."
    chmod u+w "${PHIL_CELLAR}/philotic-web" "${PHIL_CELLAR}/phil" 2>/dev/null || true
    cp target/release/philotic-web "${PHIL_CELLAR}/philotic-web"
    cp target/release/philotic-web "${PHIL_CELLAR}/phil"
    chmod u-w "${PHIL_CELLAR}/philotic-web" "${PHIL_CELLAR}/phil"
    echo "  ✓ phil"
    echo "✅ Local Homebrew install updated."

# Build release binaries locally (MacBook Air) and push them to mbp-jane via SCP.
# mbp-jane is a separate machine — it has no repo, only runs Cellar-installed binaries.
# Stops Jane on mbp-jane, installs, restarts.
remote-homebrew-push remote hotel expected_host="":
    #!/usr/bin/env bash
    set -euo pipefail
    exec ./scripts/push-homebrew-remote.sh "{{remote}}" "{{hotel}}" "{{expected_host}}"

# Stop a macOS hotel. DUAL-SUPERVISION HAZARD: mbp-jane / mac-jane run aiua under
# a launchd LaunchAgent (KeepAlive=true). A bare `pkill` there just trips a
# KeepAlive respawn — the process comes right back. So when a plist manages this
# hotel, `bootout` the launchd service (stops it AND keeps it stopped); only
# `pkill` on non-launchd (hand-started) hosts.
remote-homebrew-stop remote hotel:
    #!/usr/bin/env bash
    ssh "{{remote}}" "uid=\$(id -u); LABEL=com.philotic.aiua.{{hotel}}; \
      if [ -f \"\$HOME/Library/LaunchAgents/\${LABEL}.plist\" ]; then \
        launchctl bootout gui/\${uid}/\${LABEL} 2>/dev/null && echo '▶ launchd \${LABEL} booted out' || echo '▶ launchd \${LABEL} was not loaded'; \
      else \
        pkill -f '[a]iua --hotel {{hotel}}' 2>/dev/null && echo '▶ aiua stopped for hotel {{hotel}}' || echo '▶ aiua was not running for hotel {{hotel}}'; \
      fi"

# Start a macOS hotel. DUAL-SUPERVISION HAZARD: mbp-jane / mac-jane run aiua under
# launchd (KeepAlive=true, RunAtLoad=true). Hand-starting with nohup on top of that
# spawns a SECOND aiua that fights launchd's copy over the same IPC socket + mesh
# port → crashes / respawn-budget exhaustion (the historical incident). So restart
# THROUGH launchd when a plist exists (kickstart -k if loaded, else bootstrap so
# RunAtLoad starts it); only hand-start via nohup when NO plist exists.
remote-homebrew-start remote hotel:
    #!/usr/bin/env bash
    profile="{{hotel}}"
    if [[ "{{hotel}}" == "mbp-jane" || "{{hotel}}" == "mac-jane" ]]; then profile="jane"; fi
    if [[ "{{hotel}}" == "local-telegram" || "{{hotel}}" == "bjork" ]]; then profile="bjork"; fi
    ssh "{{remote}}" "uid=\$(id -u); LABEL=com.philotic.aiua.{{hotel}}; PLIST=\"\$HOME/Library/LaunchAgents/\${LABEL}.plist\"; \
      if [ -f \"\$PLIST\" ]; then \
        if launchctl print gui/\${uid}/\${LABEL} >/dev/null 2>&1; then \
          launchctl kickstart -k gui/\${uid}/\${LABEL} && echo '▶ \${LABEL} kickstarted under launchd'; \
        else \
          launchctl bootstrap gui/\${uid} \"\$PLIST\" 2>/dev/null || true; echo '▶ \${LABEL} bootstrapped under launchd (RunAtLoad starts it)'; \
        fi; \
      else \
        mkdir -p ~/.philotic/${profile}/graphs; ulimit -n 65536; \
        nohup env PHILOTIC_PROFILE=${profile} PHILOTIC_GRAPH_DATABASE_DIR=\$HOME/.philotic/${profile}/graphs PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE=vps-jane-aiua-01 PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE=vps-jane-aiua-01 PHILOTIC_ENABLE_RUST_AUTH=1 PHILOTIC_ENABLE_RUST_DISPATCHER=1 PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 /opt/homebrew/bin/aiua --hotel {{hotel}} >> ~/.philotic/${profile}/aiua.log 2>&1 & echo \$! > ~/.philotic/${profile}/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/${profile}/aiua.pid); \
      fi"

remote-homebrew-status remote hotel:
    @ssh "{{remote}}" "ps aux | grep '[/]opt/homebrew/bin/aiua --hotel {{hotel}}' || echo 'aiua is not running for hotel {{hotel}} on {{remote}}'"

jane-push:
    just remote-homebrew-push mbp-jane mbp-jane Jareds-MacBook-Pro

# Stop Jane on mbp-jane without pushing new binaries.
jane-stop:
    just remote-homebrew-stop mbp-jane mbp-jane

# Start Jane on mbp-jane (without pushing — uses whatever binary is already installed).
jane-start:
    just remote-homebrew-start mbp-jane mbp-jane

# Check whether Jane (aiua) is running on mbp-jane.
jane-status:
    @just remote-homebrew-status mbp-jane mbp-jane

# ── mac-jane (LOCAL Air hotel) lifecycle ────────────────────────────────────
# mac-jane runs on THIS machine under launchd. Do NOT use remote-homebrew-start
# mac-jane — {{remote}} would be "mac-jane", which sshes to an unresolvable host
# (255). These drive the local launchd job directly.
mac-jane-stop:
    #!/usr/bin/env bash
    set -euo pipefail
    LABEL=com.philotic.aiua.mac-jane; uid=$(id -u)
    launchctl bootout gui/${uid}/${LABEL} 2>/dev/null || true
    # Wait for full exit — a graceful drain can take ~30s, and a launchd/DB
    # maintenance step must not race a still-draining process.
    for _ in $(seq 1 40); do pgrep -f 'aiua --hotel mac-jane' >/dev/null || break; sleep 1; done
    if pgrep -f 'aiua --hotel mac-jane' >/dev/null; then echo "⚠ aiua --hotel mac-jane still running after 40s"; exit 1; fi
    echo "▶ mac-jane stopped (launchd job booted out)"

mac-jane-start:
    #!/usr/bin/env bash
    set -euo pipefail
    LABEL=com.philotic.aiua.mac-jane; uid=$(id -u); PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
    if launchctl print gui/${uid}/${LABEL} >/dev/null 2>&1; then
      launchctl kickstart -k gui/${uid}/${LABEL} && echo "▶ mac-jane kickstarted under launchd"
    else
      launchctl bootstrap gui/${uid} "$PLIST" && echo "▶ mac-jane bootstrapped under launchd (RunAtLoad starts it)"
    fi

# Restart mac-jane cleanly (full stop + start), e.g. to load a freshly-installed
# binary after `just local-push`.
mac-jane-restart:
    just mac-jane-stop
    just mac-jane-start

# Check whether mac-jane (aiua) is running locally.
mac-jane-status:
    @A=$(pgrep -f 'aiua --hotel mac-jane'|head -1); if [ -n "$A" ]; then echo "mac-jane aiua running: pid $A (launchd job: $(launchctl print gui/$(id -u)/com.philotic.aiua.mac-jane 2>/dev/null|grep -oE 'pid = [0-9]+'|head -1))"; else echo "mac-jane aiua is NOT running"; fi

# ── Disk-space watch ────────────────────────────────────────────────────────
# Install a launchd StartInterval job that runs scripts/disk-space-watch.sh
# (which runs `phil doctor` and alerts when system.disk-space fires). Turns the
# on-demand doctor check into an active guard so a filling disk is caught BEFORE
# ENOSPC wedges the hotel. Alert-only — never deletes anything.
disk-watch-install profile="bjork" interval="1800":
    #!/usr/bin/env bash
    set -euo pipefail
    LABEL=com.philotic.diskspacewatch
    PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
    SCRIPT="{{justfile_directory()}}/scripts/disk-space-watch.sh"
    chmod +x "$SCRIPT"
    ALERT_LOG="$HOME/.philotic/{{profile}}/disk-space-alerts.log"
    mkdir -p "$(dirname "$ALERT_LOG")"
    cat > "$PLIST" <<EOF
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0"><dict>
      <key>Label</key><string>${LABEL}</string>
      <key>ProgramArguments</key>
      <array>
        <string>/bin/bash</string>
        <string>${SCRIPT}</string>
        <string>{{profile}}</string>
      </array>
      <key>EnvironmentVariables</key>
      <dict>
        <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
        <key>PHIL_BIN</key><string>/opt/homebrew/bin/phil</string>
      </dict>
      <key>StartInterval</key><integer>{{interval}}</integer>
      <key>RunAtLoad</key><true/>
      <key>StandardErrorPath</key><string>${ALERT_LOG}</string>
      <key>StandardOutPath</key><string>/dev/null</string>
    </dict></plist>
    EOF
    uid=$(id -u)
    launchctl bootout gui/${uid}/${LABEL} 2>/dev/null || true
    launchctl bootstrap gui/${uid} "$PLIST"
    echo "▶ installed ${LABEL}: runs phil doctor every {{interval}}s (profile {{profile}}), alerts → ${ALERT_LOG}"

# Remove the disk-space watch launchd job.
disk-watch-uninstall:
    #!/usr/bin/env bash
    set -euo pipefail
    LABEL=com.philotic.diskspacewatch
    uid=$(id -u)
    launchctl bootout gui/${uid}/${LABEL} 2>/dev/null && echo "▶ ${LABEL} removed" || echo "▶ ${LABEL} was not loaded"
    rm -f "$HOME/Library/LaunchAgents/${LABEL}.plist"

# ── Logs (in-app daily rolling appender) ────────────────────────────────────
# aiua now owns rotation: detailed logs live in ~/.philotic/<profile>/logs/
# aiua.<date>.log (see crates/aiua/README.md). These recipes tail the newest
# dated file. Retention: PHILOTIC_LOG_RETENTION_DAYS (default 14 days).

# Tail the newest dated aiua log for a local profile (default: bjork).
logs profile="bjork":
    tail -f "$(ls -t ~/.philotic/{{profile}}/logs/aiua.*.log 2>/dev/null | head -1)"

# Tail the newest dated aiua log on mbp-jane (jane profile).
jane-logs:
    ssh mbp-jane 'tail -f "$(ls -t ~/.philotic/jane/logs/aiua.*.log 2>/dev/null | head -1)"'

# Tail the newest dated aiua log on vps-jane (default profile).
vps-logs:
    ssh deploy@jane-vps 'tail -f "$(ls -t ~/.philotic/default/logs/aiua.*.log 2>/dev/null | head -1)"'

# ── VPS deploy (vps-jane / Linux x86_64 via Ansible) ────────────────────────
# Strategy: rsync source to VPS → build there (VPS has rustup) → ansible
# deploys binaries from the VPS build output and restarts the systemd service.
# Prerequisites: SSH key at ~/.ssh/vps_deploy_key, vault pass at ~/.philotic-vault-pass

# Full deploy to vps-jane: sync source, build on VPS, ansible config + service.
vps-push:
    #!/usr/bin/env bash
    set -euo pipefail
    ROOT_DIR="{{justfile_directory()}}"
    VPS="${PHILOTIC_VPS_SSH_TARGET:-deploy@jane-vps}"
    VPS_CODE="/home/deploy/code/philotic-stack"
    VPS_BUILD="${VPS_CODE}/target/release"

    echo "▶ Syncing source to ${VPS}:${VPS_CODE}..."
    rsync -az --delete --checksum --no-times \
      --exclude='.git' \
      --exclude='target/' \
      --exclude='dist/' \
      --exclude='*.db' \
      --exclude='.claude/' \
      "${ROOT_DIR}/" "${VPS}:${VPS_CODE}/"

    echo "▶ Building release on ${VPS} (this may take a few minutes)..."
    ssh -n "${VPS}" "cd '${VPS_CODE}' && \$HOME/.cargo/bin/cargo build --release --bins \
      -p aiua \
      -p philote \
      -p membrane-telegram \
      -p membrane-discord \
      -p membrane-mcp \
      -p model-router \
      -p tool-runner \
      -p graph-datasource \
      -p table-datasource \
      -p router-listener \
      -p agent-datasource \
      -p heal-dispatcher \
      -p data-memorygraphrag"

    echo "▶ Deploying via ansible (binaries from VPS build at ${VPS_BUILD})..."
    cd "${ROOT_DIR}/ansible" && ansible-playbook \
      -i inventory/hosts.ini \
      deploy_hotel.yml \
      --limit jane-vps \
      --extra-vars "philotic_artifacts_remote=true philotic_artifacts_dir=${VPS_BUILD}"

# Config-only push to vps-jane: re-render mesh-config + secrets, restart service.
# Does NOT rebuild or copy binaries — uses whatever is already in /opt/philotic/bin.
vps-config:
    cd ansible && ansible-playbook -i inventory/hosts.ini deploy_hotel.yml --limit jane-vps --skip-tags binary

# CI deploy to vps-jane: fetch the latest develop build-linux artifact and ship
# it. No compilation anywhere — this replaces `vps-push` and avoids the VPS
# OOM-killing release links (it has no swap and ~2 GB free). Requires the gh CLI
# authed and a successful build-linux run on develop (.github/workflows/build-linux.yml).
#
# Default transfer path: the VPS pulls the artifact zip DIRECTLY from GitHub
# (datacenter bandwidth, seconds) instead of downloading it locally and rsyncing
# ~1.2GB over a residential uplink (measured ~37 KB/s, 4.5h+ unfinished on
# 2026-07-03). Set PHILOTIC_VPS_DEPLOY_VIA_RSYNC=1 to fall back to the old
# local-download + rsync path (for when the VPS cannot reach GitHub).
vps-deploy-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    ROOT_DIR="{{justfile_directory()}}"
    VPS="${PHILOTIC_VPS_SSH_TARGET:-deploy@jane-vps}"
    REMOTE_DIR="/home/deploy/ci-artifacts"
    STAGE="${ROOT_DIR}/dist-ci"
    REPO="${PHILOTIC_GH_REPO:-likesjx/philotic-stack}"
    SSH_OPTS=(-o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)

    echo "▶ Finding latest successful build-linux run on develop..."
    RUN_ID=$(gh run list --workflow=build-linux.yml --branch develop --status success --limit 1 --json databaseId -q '.[0].databaseId')
    if [ -z "${RUN_ID}" ]; then echo "✗ no successful build-linux run on develop — push to develop or run the workflow first"; exit 1; fi
    echo "  run ${RUN_ID}"

    if [ "${PHILOTIC_VPS_DEPLOY_VIA_RSYNC:-0}" = "1" ]; then
      echo "▶ Fallback path (PHILOTIC_VPS_DEPLOY_VIA_RSYNC=1): downloading linux-x86_64 artifact locally..."
      rm -rf "${STAGE}" && mkdir -p "${STAGE}"
      gh run download "${RUN_ID}" --name linux-x86_64 --dir "${STAGE}"
      chmod +x "${STAGE}"/* 2>/dev/null || true   # upload-artifact drops the +x bit
      echo "  $(ls -1 "${STAGE}" | grep -vc SHA256SUMS) binaries staged"

      echo "▶ Syncing binaries to ${VPS}:${REMOTE_DIR} (rsync over local uplink — slow)..."
      ssh -n "${SSH_OPTS[@]}" "${VPS}" "mkdir -p '${REMOTE_DIR}'"
      rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "${STAGE}/" "${VPS}:${REMOTE_DIR}/"
    else
      echo "▶ Resolving linux-x86_64 artifact id for run ${RUN_ID}..."
      ARTIFACT_ID=$(gh api "repos/${REPO}/actions/runs/${RUN_ID}/artifacts" -q '.artifacts[] | select(.name == "linux-x86_64") | .id')
      if [ -z "${ARTIFACT_ID}" ]; then echo "✗ run ${RUN_ID} has no linux-x86_64 artifact (expired?)"; exit 1; fi
      echo "  artifact ${ARTIFACT_ID}"

      echo "▶ Capturing short-lived signed download URL..."
      ZIP_URL=$(curl -sI -H "Authorization: Bearer $(gh auth token)" \
        "https://api.github.com/repos/${REPO}/actions/artifacts/${ARTIFACT_ID}/zip" \
        | grep -i '^location:' | tr -d '\r' | awk '{print $2}')
      if [ -z "${ZIP_URL}" ]; then echo "✗ could not resolve artifact redirect URL (gh auth token valid?) — or set PHILOTIC_VPS_DEPLOY_VIA_RSYNC=1 to fall back"; exit 1; fi

      echo "▶ ${VPS} pulling artifact directly from GitHub..."
      ssh "${SSH_OPTS[@]}" "${VPS}" "set -e; rm -rf '${REMOTE_DIR}' && mkdir -p '${REMOTE_DIR}' && curl -fsSL --connect-timeout 15 -o /tmp/philotic-ci-artifact.zip '${ZIP_URL}' && unzip -o -q /tmp/philotic-ci-artifact.zip -d '${REMOTE_DIR}' && chmod +x '${REMOTE_DIR}'/* && rm -f /tmp/philotic-ci-artifact.zip"

      echo "▶ Verifying SHA256SUMS on the remote..."
      if ! ssh -n "${SSH_OPTS[@]}" "${VPS}" "cd '${REMOTE_DIR}' && test -f SHA256SUMS && sha256sum -c --quiet SHA256SUMS"; then
        echo "✗ SHA256SUMS verification failed on ${VPS}:${REMOTE_DIR} — aborting before ansible"
        exit 1
      fi
      echo "  ✓ $(ssh -n "${SSH_OPTS[@]}" "${VPS}" "ls -1 '${REMOTE_DIR}' | grep -vc SHA256SUMS") binaries pulled and verified"
    fi

    echo "▶ Deploying via ansible (remote artifacts; stats-and-skips any missing)..."
    cd "${ROOT_DIR}/ansible" && ansible-playbook \
      -i inventory/hosts.ini \
      deploy_hotel.yml \
      --limit jane-vps \
      --extra-vars "philotic_artifacts_remote=true philotic_artifacts_dir=${REMOTE_DIR}"

# Check that vps-jane host_vars peer ports match the live context graph.
vps-port-drift-check:
    ./scripts/check-hotel-port-drift.py --host-vars ansible/host_vars/jane-vps.yml --ssh-target vps-jane

# Deploy to all hotel nodes: local (bjork) + mbp-jane + vps-jane.
push-all:
    just local-push
    just jane-push
    just vps-push

# Show configured Ansible inventory for deployment targets
ansible-inventory:
    cd ansible && ansible-inventory --list

# Verify Ansible can reach configured Philotic hotel targets
ansible-ping:
    cd ansible && ansible philotic_hotels -m ping

# Preview Philotic hotel deployment changes without applying them
ansible-check:
    cd ansible && ansible-playbook deploy_hotel.yml --check --diff

# Deploy the Philotic hotel playbook to configured hosts
ansible-deploy:
    cd ansible && ansible-playbook deploy_hotel.yml

# ── Intel Graph (Semantic Intelligence) ─────────────────────────────────────────

# Install and build intel-graph components
intel-graph-install:
    ./scripts/setup-intel-graph.sh install

# Start the intel-graph stack (ONNX sidecar + graph intelligence)
intel-graph-start:
    ./scripts/setup-intel-graph.sh start

# Start with 768-dim embedding model (higher quality, slower)
intel-graph-start-768:
    PHILOTIC_EMBED_DIM=768 ./scripts/setup-intel-graph.sh start

# Start with custom embedding model
intel-graph-start-custom model:
    PHILOTIC_EMBED_MODEL={{model}} ./scripts/setup-intel-graph.sh start

# Stop the intel-graph stack
intel-graph-stop:
    ./scripts/setup-intel-graph.sh stop

# Restart the intel-graph stack
intel-graph-restart:
    ./scripts/setup-intel-graph.sh restart

# Check intel-graph stack status
intel-graph-status:
    ./scripts/setup-intel-graph.sh status

# Start intel-graph only if not already running (idempotent, for scripts/agents)
intel-graph-ensure:
    ./scripts/setup-intel-graph.sh ensure

# Start intel-graph with agent timeout (auto-shutdown after N minutes)
intel-graph-agent timeout_minutes="60":
    ./scripts/setup-intel-graph.sh agent-start {{timeout_minutes}}

# Tail intel-graph logs
intel-graph-logs:
    ./scripts/setup-intel-graph.sh logs

# Health check both services
intel-graph-health:
    @echo "ONNX Sidecar: $(curl -s http://127.0.0.1:11435/api/health 2>/dev/null && echo '✓' || echo '✗')"
    @echo "Graph Intel:  $(curl -s http://127.0.0.1:8900/api/nodes 2>/dev/null | head -1 && echo '✓' || echo '✗')"

# Quick embed a proposal node
intel-graph-embed node_id:
    curl -s -X POST http://127.0.0.1:8901/mcp -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_embed","arguments":{"node_id":"{{node_id}}"}},"id":1}' | jq .result.content[0].text

# Batch embed all proposals
intel-graph-embed-proposals:
    curl -s -X POST http://127.0.0.1:8901/mcp -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_embed_batch","arguments":{"kind":"proposal"}},"id":1}' | jq '.result | {processed, embedded}'

# Run the full agent workflow smoke (scan → next_task → context → session → decide → close → dashboard)
smoke-agent-workflow:
    bash scripts/smoke-agent-workflow.sh

# Run tests and record results to the graph against an explicit target_id
# (thin wrapper over scripts/test-and-record.sh, which now backs `just test`
# by default — kept for explicit target overrides / CI call sites).
test-and-record target_id:
    GRAPH_TEST_TARGET={{target_id}} ./scripts/test-and-record.sh

# Combined system health check (sessions + proposals + graph stats)
intel-graph-health-check:
    @echo "── Intel Graph System Health ──"
    @curl -s http://127.0.0.1:8900/api/health 2>/dev/null | jq . || echo "Graph server not running"

# Session health report
intel-graph-session-health:
    @curl -s http://127.0.0.1:8900/api/health/sessions 2>/dev/null | jq . || echo "Graph server not running"

# Auto-close stale sessions (default: older than 4 hours)
intel-graph-session-cleanup max_age_hours="4":
    @echo "Cleaning stale sessions (max age: {{max_age_hours}}h)..."
    @curl -s -X POST http://127.0.0.1:8900/api/session/cleanup \
      -H "Content-Type: application/json" \
      -d '{"max_age_hours": {{max_age_hours}}}' 2>/dev/null | jq . || echo "Graph server not running"

# Proposal pipeline health report
intel-graph-proposal-health:
    @curl -s http://127.0.0.1:8900/api/health/proposals 2>/dev/null | jq . || echo "Graph server not running"

# Embed all embeddable node kinds (proposals, seams, tasks, functions, types, modules, tests)
intel-graph-embed-all:
    #!/usr/bin/env bash
    set -euo pipefail
    for kind in proposal seam task function type module test; do
        echo "Embedding ${kind}s..."
        curl -s -X POST http://127.0.0.1:8901/mcp \
          -H "Content-Type: application/json" \
          -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"graph_embed_batch\",\"arguments\":{\"kind\":\"${kind}\"}},\"id\":1}" | jq -r '.result.content[0].text' 2>/dev/null || echo "  failed for ${kind}"
    done

# Full graph maintenance: scan + cleanup + health check
intel-graph-maintain:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "── Step 1: Scan ──"
    curl -s -X POST http://127.0.0.1:8900/api/scan | jq '{crates, modules, types, functions, tests, docs, duration_ms}' 2>/dev/null || echo "Scan failed"
    echo ""
    echo "── Step 2: Session Cleanup ──"
    curl -s -X POST http://127.0.0.1:8900/api/session/cleanup | jq . 2>/dev/null || echo "Cleanup failed"
    echo ""
    echo "── Step 3: Health Check ──"
    curl -s http://127.0.0.1:8900/api/health | jq . 2>/dev/null || echo "Health check failed"
    echo ""
    echo "── Step 4: Embed Proposals ──"
    curl -s -X POST http://127.0.0.1:8901/mcp \
      -H "Content-Type: application/json" \
      -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_embed_batch","arguments":{"kind":"proposal"}},"id":1}' | jq -r '.result.content[0].text' 2>/dev/null || echo "Embedding failed"
    echo ""
    echo "✅ Maintenance complete."

# Semantic search
intel-graph-search query limit="10":
    curl -s -X POST http://127.0.0.1:8901/mcp -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_semantic_search","arguments":{"query":"{{query}}","limit":{{limit}}}},"id":1}' | jq '.result.results | map({name, kind, similarity})'

# Open Web UI in browser (macOS)
intel-graph-ui:
    open http://127.0.0.1:8900

# Rebuild the graphify tree-sitter code graph (offline, no LLM)
graphify-update:
    graphify update .

# Watch the repo and rebuild the graphify graph on code changes
graphify-watch:
    graphify watch .

# Query the graphify graph: shortest path between two nodes
graphify-path a b:
    graphify path "{{a}}" "{{b}}"

# Explain a node and its neighbors in the graphify graph
graphify-explain node:
    graphify explain "{{node}}"

# Bridge graphify call edges into the intel-graph (run after graphify-update)
graphify-bridge:
    python3 scripts/graphify_bridge.py

# Close active workstream with summary and disposition
close-workstream:
    #!/usr/bin/env bash
    set -euo pipefail
    
    # Get active workstream
    WORKSTREAM=$(curl -s "http://127.0.0.1:8900/api/nodes?kind=workstream" | jq -r '.[] | select(.properties.status == "active") | .id' | head -1)
    if [ -z "$WORKSTREAM" ]; then
        echo "No active workstream found"
        exit 1
    fi
    
    # Get session
    SESSION=$(curl -s "http://127.0.0.1:8900/api/nodes?kind=session" | jq -r '.[] | select(.properties.status == "active") | .id' | head -1)
    
    echo "Closing workstream: $WORKSTREAM"
    echo "Session: ${SESSION:-none}"
    echo ""
    
    # Prompt for details
    read -p "Disposition (completed/partial/blocked/superseded/cancelled): " DISPOSITION
    DISPOSITION=${DISPOSITION:-completed}
    
    read -p "Verification level (none/test-green/smoke-green/watched-live-green): " VERIFIED
    if [ "$DISPOSITION" = "completed" ] && [ -z "$VERIFIED" ]; then
        echo "Completed workstreams must include a verification level"
        exit 1
    fi
    VERIFIED=${VERIFIED:-none}
    
    read -p "Summary of work: " SUMMARY
    
    # Close workstream
    curl -s -X POST "http://127.0.0.1:8900/api/nodes/${WORKSTREAM}/update" \
      -H "Content-Type: application/json" \
      -d "{\"properties\":{\"status\":\"closed\",\"disposition\":\"${DISPOSITION}\",\"verified\":\"${VERIFIED}\",\"summary\":\"${SUMMARY}\",\"end_time\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}}" | jq '.updated'
    
    # Close session if exists
    if [ -n "$SESSION" ]; then
        curl -s -X POST http://127.0.0.1:8901/mcp \
          -H "Content-Type: application/json" \
          -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"session_close\",\"arguments\":{\"session_id\":\"${SESSION}\",\"status\":\"${DISPOSITION}\",\"verified\":\"${VERIFIED}\",\"summary\":\"${SUMMARY}\"}},\"id\":1}" | jq -r '.result.content[0].text'
    fi
    
    echo ""
    echo "✅ Workstream closed"
