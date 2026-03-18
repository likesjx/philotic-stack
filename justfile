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
    cargo run -p aiua -- --hotel {{hotel}} --load-config mesh-config.json

# Rebuild the local runtime binaries that the hotel materializes during watched UAT.
build-runtime:
    cargo build -p aiua -p philote -p membrane -p model-router

# Kill local Philotic hotel/guest binaries from this checkout and clear stale sockets.
kill-local-stack:
    @pkill -f "/Users/jaredlikes/code/philotic-stack/target/debug/aiua" || true
    @pkill -f "/Users/jaredlikes/code/philotic-stack/target/debug/membrane" || true
    @pkill -f "/Users/jaredlikes/code/philotic-stack/target/debug/philote" || true
    @pkill -f "/Users/jaredlikes/code/philotic-stack/target/debug/model-controller-gemini" || true
    @pkill -f "/Users/jaredlikes/code/philotic-stack/target/debug/model-controller-elevenlabs" || true
    @pkill -f "/Users/jaredlikes/code/philotic-stack/target/debug/tool-runner" || true
    @rm -f /tmp/philotic-default.sock /tmp/philotic-local-telegram.sock /tmp/philotic-aria-architect-hotel.sock /tmp/philotic-startup-test-hotel.sock

# Rebuild first, then kill stale local runtime processes/sockets, then start one hotel cleanly.
start-aiua-clean hotel:
    just build-runtime
    just kill-local-stack
    cargo run -p aiua -- --hotel {{hotel}} --load-config mesh-config.json

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
    cargo run -p aiua -- --hotel local-telegram --load-config mesh-config.json

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

# Run the structured cognitive startup smoke
smoke-cognitive:
    bash scripts/smoke-cognitive-roundtrip.sh

# Run the ONNX embedding sidecar smoke (downloads model on first run, ~300 MB)
smoke-embed:
    bash scripts/smoke-embed-roundtrip.sh

# Run the trusted vertical-slice verification suite
verify-vertical-slice:
    cargo test -p philotic-client -- --nocapture
    cargo test -p philote -- --nocapture
    cargo test -p aiua -- --nocapture
    ./scripts/smoke-routed-tool-roundtrip.sh
    ./scripts/smoke-approval-roundtrip.sh
    ./scripts/smoke-session-control-roundtrip.sh
    ./scripts/smoke-session-bindings-roundtrip.sh

# Print the operator checklist for the current trusted vertical slice
operator-checklist:
    @echo "Philotic Vertical Slice Operator Checklist"
    @echo ""
    @echo "1. Run: just verify-vertical-slice"
    @echo "2. If you need extra approval-path confidence, also run:"
    @echo "   - just smoke-deny"
    @echo "   - just smoke-approve-steer"
    @echo "   - just smoke-deny-redirect"
    @echo "   - just smoke-preapprove"
    @echo "3. For watched-live confidence, start a hotel and verify:"
    @echo "   - guest processes are alive"
    @echo "   - guests actually register/subscribe"
    @echo "   - routed tool flow succeeds"
    @echo "4. Record the highest honest confidence level:"
    @echo "   - test-green"
    @echo "   - smoke-green"
    @echo "   - watched-live-green"
    @echo "5. Note any assumption-vs-reality gaps before closing the slice"

# Build release binaries and hot-push them into the Homebrew Cellar (mbp-jane).
# Stops Jane, installs, restarts with PHILOTIC_PROFILE=jane.
jane-push:
    #!/usr/bin/env bash
    set -euo pipefail
    CELLAR=/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin
    BINS="aiua philote membrane model-router model-controller-gemini model-controller-elevenlabs philote-worker tool-runner"
    echo "▶ Building release binaries..."
    cargo build --release -p aiua -p philote -p membrane -p model-router -p tool-runner
    echo "▶ Stopping Jane..."
    PHILOTIC_PROFILE=jane phil stop 2>/dev/null || true
    sleep 1
    echo "▶ Installing binaries into Cellar..."
    for bin in $BINS; do
        if [ -f target/release/$bin ]; then
            chmod u+w "$CELLAR/$bin"
            cp target/release/$bin "$CELLAR/$bin"
            chmod u-w "$CELLAR/$bin"
            ln -sf "$CELLAR/$bin" /opt/homebrew/bin/$bin
            echo "  ✓ $bin"
        fi
    done
    echo "▶ Starting Jane..."
    PHILOTIC_PROFILE=jane phil start --hotel default
    echo "✅ Jane updated and running."

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
