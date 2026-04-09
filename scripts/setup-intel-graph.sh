#!/bin/bash
# Intel Graph Setup Script
# Sets up the Philotic Stack intel-graph for local development
# Usage: ./scripts/setup-intel-graph.sh [start|stop|status|install]

set -e

PHILOTIC_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA_DIR="${HOME}/.local/share/philotic"
MODEL_CACHE="${DATA_DIR}/models"
PID_DIR="${DATA_DIR}/pids"
LOG_DIR="${DATA_DIR}/logs"
GRAPH_DB="${DATA_DIR}/graph.db"
ONNX_PORT=11435
GRAPH_PORT=8900

# Embedding model configuration
# Options: 384 (default, fast), 768 (higher quality)
# Or set PHILOTIC_EMBED_MODEL to a custom HuggingFace repo
EMBED_DIM="${PHILOTIC_EMBED_DIM:-384}"

case "$EMBED_DIM" in
    384)
        EMBED_MODEL="${PHILOTIC_EMBED_MODEL:-sentence-transformers/all-MiniLM-L6-v2}"
        ;;
    768)
        EMBED_MODEL="${PHILOTIC_EMBED_MODEL:-Xenova/all-mpnet-base-v2}"
        ;;
    *)
        EMBED_MODEL="${PHILOTIC_EMBED_MODEL:-sentence-transformers/all-MiniLM-L6-v2}"
        warn "Unknown PHILOTIC_EMBED_DIM=$EMBED_DIM, using default 384-dim model"
        ;;
esac

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log() {
    echo -e "${BLUE}[intel-graph]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[warning]${NC} $1"
}

error() {
    echo -e "${RED}[error]${NC} $1"
}

success() {
    echo -e "${GREEN}[success]${NC} $1"
}

ensure_dirs() {
    mkdir -p "$DATA_DIR" "$MODEL_CACHE" "$PID_DIR" "$LOG_DIR"
}

install_check() {
    log "Checking prerequisites..."
    
    # Check Rust
    if ! command -v cargo &> /dev/null; then
        error "Rust/Cargo not found. Install from https://rustup.rs/"
        exit 1
    fi
    success "Rust: $(cargo --version)"
    
    # Check if we're in the right place
    if [[ ! -f "$PHILOTIC_ROOT/Cargo.toml" ]]; then
        error "Not in philotic-stack directory. Run from repo root."
        exit 1
    fi
    
    success "Repository: $PHILOTIC_ROOT"
}

build_components() {
    log "Building components..."
    
    cd "$PHILOTIC_ROOT"
    
    # Build graph-intelligence
    log "Building graph-intelligence..."
    cargo build --release -p graph-intelligence 2>&1 | tail -5
    
    # Build model-router with onnx binary
    log "Building model-controller-onnx..."
    cargo build --release -p model-router --bin model-controller-onnx 2>&1 | tail -5
    
    success "Build complete"
}

download_model() {
    log "Setting up embedding model..."
    
    # The model will be downloaded automatically by the onnx-runner on first use
    # via HuggingFace Hub. We just verify the cache directory is ready.
    
    if [[ ! -d "$MODEL_CACHE/${PHILOTIC_ONNX_EMBED_REPO:-"nomic-ai/nomic-embed-text-v1.5"}" ]]; then
        warn "Model not cached. Will download on first startup (~600MB)"
        warn "This may take a few minutes on first run."
    else
        success "Model cached: nomic-embed-text-v1.5"
    fi
}

live_pid_for_port() {
    local port="$1"
    local pid
    pid=$(lsof -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | head -1)
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        echo "$pid"
        return 0
    fi
    return 1
}

running_pid() {
    local pid_file="$1"
    local port="$2"

    if [[ -f "$pid_file" ]]; then
        local pid=$(cat "$pid_file" 2>/dev/null)
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            echo "$pid"
            return 0
        fi
    fi

    if pid=$(live_pid_for_port "$port"); then
        echo "$pid" > "$pid_file"
        echo "$pid"
        return 0
    fi

    return 1
}

start_onnx() {
    if pid=$(running_pid "$PID_DIR/onnx.pid" "$ONNX_PORT"); then
        warn "ONNX sidecar already running (PID: $pid)"
        return 0
    fi
    
    log "Starting ONNX model controller on port $ONNX_PORT..."
    log "Embedding model: $EMBED_MODEL (${EMBED_DIM}-dim)"
    
    cd "$PHILOTIC_ROOT"
    nohup ~/.cargo/bin/cargo run --release -p model-router --bin model-controller-onnx -- \
        --sidecar-only \
        --embed-repo "$EMBED_MODEL" \
        > "$LOG_DIR/onnx.log" 2>&1 &
    
    local pid=$!
    echo $pid > "$PID_DIR/onnx.pid"
    
    # Wait for health check
    log "Waiting for ONNX sidecar to be ready..."
    local retries=30
    while [[ $retries -gt 0 ]]; do
        if curl -s "http://127.0.0.1:$ONNX_PORT/health" > /dev/null 2>&1; then
            success "ONNX sidecar ready (PID: $pid)"
            return 0
        fi
        sleep 1
        ((retries--))
    done
    
    error "ONNX sidecar failed to start. Check $LOG_DIR/onnx.log"
    return 1
}

start_graph() {
    if pid=$(running_pid "$PID_DIR/graph.pid" "$GRAPH_PORT"); then
        warn "Graph intelligence already running (PID: $pid)"
        return 0
    fi
    
    log "Starting graph intelligence on port $GRAPH_PORT..."
    log "Database: $GRAPH_DB"
    
    cd "$PHILOTIC_ROOT"
    export PHILOTIC_GRAPH_DB="$GRAPH_DB"
    
    nohup ~/.cargo/bin/cargo run --release -p graph-intelligence -- \
        --port $GRAPH_PORT \
        --mcp-port $((GRAPH_PORT + 1)) \
        --db "$GRAPH_DB" \
        --worktree "$PHILOTIC_ROOT" \
        > "$LOG_DIR/graph.log" 2>&1 &
    
    local pid=$!
    echo $pid > "$PID_DIR/graph.pid"
    
    # Wait for health check
    log "Waiting for graph intelligence to be ready..."
    local retries=30
    while [[ $retries -gt 0 ]]; do
        if curl -s "http://127.0.0.1:$GRAPH_PORT/api/health" > /dev/null 2>&1; then
            success "Graph intelligence ready (PID: $pid)"
            success "Web UI: http://127.0.0.1:$GRAPH_PORT"
            return 0
        fi
        sleep 1
        ((retries--))
    done
    
    error "Graph intelligence failed to start. Check $LOG_DIR/graph.log"
    return 1
}

start_all() {
    ensure_dirs
    
    log "Starting Intel Graph stack..."
    
    start_onnx
    start_graph
    
    success "Intel Graph stack is running!"
    log ""
    log "Next steps:"
    log "  1. Open Web UI: http://127.0.0.1:$GRAPH_PORT"
    log "  2. Go to 'Search' tab for semantic search"
    log "  3. Run: curl -X POST http://127.0.0.1:$GRAPH_PORT/mcp -d '{jsonrpc:2.0,method:tools/call,params:{name:graph_embed_batch,arguments:{kind:proposal}}}'"
    log ""
    log "Status: $0 status"
    log "Stop:   $0 stop"
}

stop_all() {
    log "Stopping Intel Graph stack..."
    
    if pid=$(running_pid "$PID_DIR/graph.pid" "$GRAPH_PORT"); then
        log "Stopping graph intelligence (PID: $pid)..."
        kill "$pid" 2>/dev/null || true
        rm -f "$PID_DIR/graph.pid"
    fi
    
    if pid=$(running_pid "$PID_DIR/onnx.pid" "$ONNX_PORT"); then
        log "Stopping ONNX sidecar (PID: $pid)..."
        kill "$pid" 2>/dev/null || true
        rm -f "$PID_DIR/onnx.pid"
    fi
    
    success "Intel Graph stack stopped"
}

status() {
    local onnx_status="${RED}stopped${NC}"
    local graph_status="${RED}stopped${NC}"
    
    if pid=$(running_pid "$PID_DIR/onnx.pid" "$ONNX_PORT"); then
        onnx_status="${GREEN}running${NC} (PID: $pid, port: $ONNX_PORT)"
    fi
    
    if pid=$(running_pid "$PID_DIR/graph.pid" "$GRAPH_PORT"); then
        graph_status="${GREEN}running${NC} (PID: $pid, port: $GRAPH_PORT)"
    fi
    
    echo ""
    echo -e "ONNX Sidecar: $onnx_status"
    echo -e "Graph Intel:  $graph_status"
    echo ""
    echo "Data directory: $DATA_DIR"
    echo "Logs:           $LOG_DIR"
    echo "Embed model:    $EMBED_MODEL (${EMBED_DIM}-dim)"
    echo ""
    
    if running_pid "$PID_DIR/graph.pid" "$GRAPH_PORT" >/dev/null; then
        echo "Web UI: http://127.0.0.1:$GRAPH_PORT"
        echo "API:    http://127.0.0.1:$GRAPH_PORT/api"
        echo "MCP:    http://127.0.0.1:$GRAPH_PORT/mcp"
    fi
}

# Timeout/killswitch handler
timeout_watchdog() {
    local timeout_minutes="${1:-60}"  # Default 60 minutes
    log "Timeout watchdog enabled (${timeout_minutes} min)"
    
    (
        sleep $((timeout_minutes * 60))
        if running_pid "$PID_DIR/graph.pid" "$GRAPH_PORT" || running_pid "$PID_DIR/onnx.pid" "$ONNX_PORT"; then
            warn "Timeout reached (${timeout_minutes} min). Shutting down..."
            stop_all
        fi
    ) &
}

# Agent-initiated startup
agent_start() {
    local timeout="${1:-60}"
    log "Agent-initiated startup (timeout: ${timeout}min)"
    
    ensure_dirs
    start_onnx
    start_graph
    timeout_watchdog "$timeout"
    
    # Output JSON status for agent parsing
    cat << EOF
{
  "status": "ready",
  "ports": {
    "onnx": $ONNX_PORT,
    "graph": $GRAPH_PORT
  },
  "web_ui": "http://127.0.0.1:$GRAPH_PORT",
  "mcp_endpoint": "http://127.0.0.1:$GRAPH_PORT/mcp",
  "timeout_minutes": $timeout
}
EOF
}

# Main
case "${1:-}" in
    install)
        install_check
        build_components
        download_model
        success "Installation complete. Run: $0 start"
        ;;
    start)
        start_all
        ;;
    stop)
        stop_all
        ;;
    status)
        status
        ;;
    restart)
        stop_all
        sleep 2
        start_all
        ;;
    agent-start)
        agent_start "${2:-60}"
        ;;
    ensure)
        # Start if not running, no-op if already running. Silent for scripting.
        ensure_dirs
        if running_pid "$PID_DIR/graph.pid" "$GRAPH_PORT" && running_pid "$PID_DIR/onnx.pid" "$ONNX_PORT"; then
            echo '{"status":"already_running","graph_port":'$GRAPH_PORT',"mcp_endpoint":"http://127.0.0.1:'$((GRAPH_PORT+1))'/mcp"}'
        else
            start_onnx
            start_graph
            echo '{"status":"started","graph_port":'$GRAPH_PORT',"mcp_endpoint":"http://127.0.0.1:'$((GRAPH_PORT+1))'/mcp"}'
        fi
        ;;
    health)
        # Quick health check for monitoring
        curl -s "http://127.0.0.1:$ONNX_PORT/health" > /dev/null 2>&1 && echo "onnx:ok" || echo "onnx:down"
        curl -s "http://127.0.0.1:$GRAPH_PORT/api/health" > /dev/null 2>&1 && echo "graph:ok" || echo "graph:down"
        ;;
    logs)
        tail -f "$LOG_DIR"/*.log
        ;;
    *)
        echo "Intel Graph Setup Script"
        echo ""
        echo "Usage: $0 <command>"
        echo ""
        echo "Commands:"
        echo "  install       - Build components and setup model cache"
        echo "  start         - Start the full stack (ONNX + Graph)"
        echo "  stop          - Stop all services"
        echo "  restart       - Restart all services"
        echo "  status        - Show service status"
        echo "  agent-start [timeout_min] - Agent-initiated startup with timeout"
        echo "  ensure        - Start if not running, no-op if already running (for scripts/agents)"
        echo "  health        - Quick health check"
        echo "  logs          - Tail all logs"
        echo ""
        echo "Environment Variables:"
        echo "  PHILOTIC_EMBED_DIM    - Embedding dimensions: 384 (default, fast) or 768 (higher quality)"
        echo "  PHILOTIC_EMBED_MODEL  - Custom HuggingFace model repo (overrides preset)"
        echo ""
        echo "Examples:"
        echo "  $0 install                              # First-time setup with 384-dim"
        echo "  $0 start                                # Start with default 384-dim model"
        echo "  PHILOTIC_EMBED_DIM=768 $0 start         # Start with 768-dim model"
        echo "  $0 agent-start 30                       # Start with 30-min timeout"
        echo ""
        echo "Model Presets:"
        echo "  384-dim: sentence-transformers/all-MiniLM-L6-v2 (fast, ~80MB)"
        echo "  768-dim: Xenova/all-mpnet-base-v2 (quality, ~420MB, ONNX-optimized)"
        echo ""
        exit 1
        ;;
esac
