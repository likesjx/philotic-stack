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
start-ansible:
    cargo run -p ansible -- --load-config mesh-config.json

# Start the Gateway (Telegram Hegemon)
start-gateway:
    cargo run -p hegemon

# Start the Persona (Agent Core)
start-agent:
    cargo run -p agent-core

# Start the Mind (Model Router)
start-model:
    cargo run -p model-router

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
