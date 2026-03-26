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
    cargo build -p aiua -p philote -p membrane -p model-router -p tool-runner -p graph-runner -p philotic-web

# Kill local Philotic hotel/guest binaries from this checkout and clear stale sockets.
kill-local-stack:
    @pkill -KILL -f "target/debug/aiua" 2>/dev/null || true
    @pkill -KILL -f "target/debug/membrane" 2>/dev/null || true
    @pkill -KILL -f "target/debug/philote" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-gemini" 2>/dev/null || true
    @pkill -KILL -f "target/debug/model-controller-elevenlabs" 2>/dev/null || true
    @pkill -KILL -f "target/debug/tool-runner" 2>/dev/null || true
    @pkill -KILL -f "target/debug/graph-runner" 2>/dev/null || true
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
    @pkill -KILL -f "agent-graph-runner" 2>/dev/null || true
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
    cargo run -p membrane

# Start the Persona (Philote)
start-agent:
    cargo run -p philote

# Start the Mind (Model Router)
start-model:
    cargo run -p model-router

# Start the Tool Runner
start-tool:
    cargo run -p tool-runner

# Start the Graph Runner (context graph + table adapter)
start-graph-runner:
    cargo run -p graph-runner

# Start the full stack in background (requires tmux or similar)
start:
    @echo "Starting the Philotic Stack..."
    @echo "To run these properly, you should run the individual start-* commands in separate panes."

# Check the status of the local mesh (pings the Aiua host daemon)
status:
    @echo "Checking Philotic Stack local status..."
    @# Ping the Aiua daemon port or check processes.
    @ps aux | grep -v grep | grep "cargo run -p aiua" || echo "Aiua daemon is not running."
    @ps aux | grep -v grep | grep "cargo run -p membrane" || echo "Membrane gateway is not running."

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

# Run tests
test:
    cargo test --workspace

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

# Run the graph-runner smoke (create → upsert node → get node → export round-trip)
smoke-graph-runner:
    bash scripts/smoke-graph-runner-roundtrip.sh

# Run the agent-graph-runner live smoke (write + declare + sync round-trip, Seams 3 & 4)
smoke-agent-graph:
    bash scripts/smoke-agent-graph-roundtrip.sh

# Run the desktop membrane management surface smoke (lease, REST API, auth, clean shutdown)
smoke-desktop-membrane:
    bash scripts/smoke-desktop-membrane.sh

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

# Run the MLX model controller smoke (requires mlx_lm installed + Apple Silicon)
smoke-mlx:
    bash scripts/smoke-mlx-controller.sh

# Run the agent-graph-runner cargo integration tests (tool dispatch without live hotel, Seams 3 & 4)
test-agent-graph:
    cargo test -p agent-graph-runner --test smoke -- --nocapture

# Run the router trace cargo tests (RouterTraceStorage unit coverage, Seam 5)
test-router-trace:
    cargo test -p ansible-mesh-core -- router_trace --nocapture

# ── UAT Suites ────────────────────────────────────────────────────────────────

# Core unit + integration test suite (no binaries, fast)
test-suite:
    cargo test -p philotic-client -- --nocapture
    cargo test -p philote -- --nocapture
    cargo test -p aiua -- --nocapture
    cargo test -p agent-graph-runner --test smoke -- --nocapture
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
    bash scripts/smoke-graph-runner-roundtrip.sh
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

# Build release binaries locally (MacBook Air) and push them to mbp-jane via SCP.
# mbp-jane is a separate machine — it has no repo, only runs Cellar-installed binaries.
# Stops Jane on mbp-jane, installs, restarts.
jane-push:
    #!/usr/bin/env bash
    set -euo pipefail
    REMOTE=mbp-jane
    REMOTE_CELLAR=/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin
    BINS="aiua philote membrane model-router model-controller-gemini model-controller-elevenlabs model-controller-mlx philote-worker tool-runner graph-runner philotic-web"
    # Safety guard: verify we are actually talking to mbp-jane before touching anything.
    # mbp-jane's system hostname is "MacBookPro" — the SSH alias is just our local label.
    ACTUAL_HOST="$(ssh "${REMOTE}" hostname -s 2>/dev/null)"
    if [ "${ACTUAL_HOST}" != "MacBookPro" ]; then
        echo "❌ Aborting: remote hostname is '${ACTUAL_HOST}', expected 'MacBookPro' (mbp-jane)."
        exit 1
    fi
    echo "▶ Building release binaries (local)..."
    cargo build --release -p aiua -p philote -p membrane -p model-router -p tool-runner -p graph-runner -p philotic-web
    echo "▶ Stopping Jane on ${REMOTE}..."
    ssh "${REMOTE}" "pkill -f '/opt/homebrew/bin/aiua' 2>/dev/null || true; sleep 2"
    echo "▶ Pushing binaries to ${REMOTE}:${REMOTE_CELLAR}..."
    for bin in $BINS; do
        if [ ! -f "target/release/$bin" ]; then
            echo "  – $bin (not built locally, skipping)"
            continue
        fi
        if ! ssh "${REMOTE}" "test -f '${REMOTE_CELLAR}/$bin'"; then
            echo "  – $bin (not in remote Cellar, skipping)"
            continue
        fi
        ssh "${REMOTE}" "chmod u+w '${REMOTE_CELLAR}/$bin'"
        scp -q "target/release/$bin" "${REMOTE}:${REMOTE_CELLAR}/$bin"
        ssh "${REMOTE}" "chmod u-w '${REMOTE_CELLAR}/$bin'"
        echo "  ✓ $bin"
    done
    echo "▶ Applying config on ${REMOTE}..."
    ssh "${REMOTE}" "/opt/homebrew/bin/aiua load --file ~/mesh-config.json --hotel default"
    echo "▶ Starting Jane on ${REMOTE}..."
    ssh "${REMOTE}" "nohup /opt/homebrew/bin/aiua --hotel default >> ~/.philotic/aiua.log 2>&1 & echo \$! > ~/.philotic/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/aiua.pid)"
    echo "✅ Jane updated and running on ${REMOTE}."

# Stop Jane on mbp-jane without pushing new binaries.
jane-stop:
    #!/usr/bin/env bash
    ssh mbp-jane "pkill -f '/opt/homebrew/bin/aiua' && echo '▶ aiua stopped' || echo '▶ aiua was not running'"

# Start Jane on mbp-jane (without pushing — uses whatever binary is already installed).
jane-start:
    #!/usr/bin/env bash
    ssh mbp-jane "nohup /opt/homebrew/bin/aiua --hotel default >> ~/.philotic/aiua.log 2>&1 & echo \$! > ~/.philotic/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/aiua.pid)"

# Check whether Jane (aiua) is running on mbp-jane.
jane-status:
    @ssh mbp-jane "ps aux | grep '[/]opt/homebrew/bin/aiua' || echo 'aiua is not running on mbp-jane'"

# Show configured Ansible inventory for deployment targets
ansible-inventory:
    cd ansible && ansible-inventory --list

# Verify Ansible can reach configured mesh nodes
ansible-ping:
    cd ansible && ansible mesh_nodes -m ping

# Preview deployment changes without applying them
ansible-check:
    cd ansible && ansible-playbook deploy_mesh_node.yml --check --diff

# Deploy the mesh node playbook to configured hosts
ansible-deploy:
    cd ansible && ansible-playbook deploy_mesh_node.yml
