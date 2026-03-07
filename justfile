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

# Start the Hotel Manager (Ansible Host Daemon)
start-ansible hotel:
    cargo build --workspace
    cargo run -p ansible -- --hotel {{hotel}} --load-config mesh-config.json

# Start the local UAT stack. Ansible will materialize the gateway, agent, model, and tool guests.
start-ansible-uat:
    @echo "Starting UAT stack on hotel 'local-telegram' using mesh-config.json..."
    @echo "If you are testing Telegram, make sure only one Telegram poller is running for this bot token."
    cargo run -p ansible -- --hotel local-telegram --load-config mesh-config.json

# Start the Gateway (Telegram Hegemon)
start-gateway:
    cargo run -p hegemon

# Start the Persona (Agent Core)
start-agent:
    cargo run -p agent-core

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

# Check the status of the local mesh (pings the Ansible host daemon)
status:
    @echo "Checking Philotic Stack local status..."
    @# Ping the Ansible daemon port or check processes.
    @ps aux | grep -v grep | grep "cargo run -p ansible" || echo "Ansible daemon is not running."
    @ps aux | grep -v grep | grep "cargo run -p hegemon" || echo "Hegemon gateway is not running."

# Format code
format:
    cargo fmt --all

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

# Run the session lifecycle/control binary smoke test
smoke-session-control:
    ./scripts/smoke-session-control-roundtrip.sh

# Run the session bindings binary smoke test
smoke-session-bindings:
    ./scripts/smoke-session-bindings-roundtrip.sh

# Run the trusted vertical-slice verification suite
verify-vertical-slice:
    cargo test -p philotic-client -- --nocapture
    cargo test -p agent-core -- --nocapture
    cargo test -p ansible -- --nocapture
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
