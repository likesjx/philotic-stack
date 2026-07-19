#!/usr/bin/env bash
# Idea sweep + triage for the Aria idea pipeline
# (docs/architecture/ARIA_IDEA_PIPELINE_PROPOSAL.md, stage 2).
#
# Operator ideas are LifeGraph GrowthHypothesis nodes with id `idea:<slug>`,
# written by Aria at intake (slice 1). Coding sessions run this at bootstrap
# to surface pending ideas, and use the triage verbs to move them through the
# lifecycle:  (absent idea_status == captured) -> promoted | declined -> shipped.
#
# TRANSITIONAL (named per AGENTS.md §2.3): status transitions are written as
# direct Memgraph cypher over ssh. That bypasses the life-graph runner, so no
# provenance envelope is attached and no LifeGraphChange push reaches the
# operator's devices — each verb stamps idea_updated_by instead. Replace the
# write path with a governed session-side life.* client when one exists (see
# the MCP client-fabric proposal). Reads are unaffected.
#
# Usage:
#   scripts/idea-sweep.sh                 # pending (captured) ideas
#   scripts/idea-sweep.sh all             # every idea node with status
#   scripts/idea-sweep.sh promote <idea:slug> <graph-ref> [by]
#   scripts/idea-sweep.sh decline <idea:slug> <reason> [by]
#   scripts/idea-sweep.sh ship    <idea:slug> [note] [by]
#
# Env:
#   PHILOTIC_MEMGRAPH_SSH   ssh host running the philotic-memgraph container
#                           (default: first reachable of jane-vps, vps-jane-tailscale)

set -euo pipefail

SSH_OPTS=(-o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=2)
CONTAINER="${PHILOTIC_MEMGRAPH_CONTAINER:-philotic-memgraph}"

resolve_ssh_host() {
  if [[ -n "${PHILOTIC_MEMGRAPH_SSH:-}" ]]; then
    echo "${PHILOTIC_MEMGRAPH_SSH}"
    return 0
  fi
  local candidate
  for candidate in jane-vps vps-jane-tailscale; do
    if ssh -n "${SSH_OPTS[@]}" -o BatchMode=yes "${candidate}" true 2>/dev/null; then
      echo "${candidate}"
      return 0
    fi
  done
  echo "❌ No reachable Memgraph ssh host (tried jane-vps, vps-jane-tailscale). Set PHILOTIC_MEMGRAPH_SSH." >&2
  return 1
}

cypher() {
  local query="$1"
  ssh "${SSH_OPTS[@]}" "${SSH_HOST}" \
    "echo '${query//\'/\'\\\'\'}' | docker exec -i ${CONTAINER} mgconsole --output-format=csv"
}

# Escape a shell string for embedding inside a cypher double-quoted literal.
cypher_str() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

require_idea_id() {
  local id="$1"
  if [[ ! "${id}" =~ ^idea:[a-z0-9][a-z0-9._-]*$ ]]; then
    echo "❌ '${id}' is not a valid idea id (expected idea:<slug>, lowercase [a-z0-9._-])" >&2
    exit 1
  fi
}

show_idea() {
  local id="$1"
  cypher "MATCH (n:GrowthHypothesis {id: \"$(cypher_str "${id}")\"}) RETURN n.id, coalesce(n.idea_status, \"captured\") AS idea_status, n.graph_ref, n.idea_status_reason, n.claim_summary;"
}

VERB="${1:-pending}"
SSH_HOST="$(resolve_ssh_host)"

case "${VERB}" in
  pending)
    echo "▶ Pending (captured) ideas — triage per \$graph-intelligence 'Idea Sweep':"
    cypher 'MATCH (n:GrowthHypothesis) WHERE n.id STARTS WITH "idea:" AND coalesce(n.idea_status, "captured") = "captured" RETURN n.id, n.claim_summary, n.observed_by, n.created_at ORDER BY n.created_at;'
    ;;
  all)
    cypher 'MATCH (n:GrowthHypothesis) WHERE n.id STARTS WITH "idea:" RETURN n.id, coalesce(n.idea_status, "captured") AS idea_status, n.graph_ref, n.claim_summary, n.created_at ORDER BY n.created_at;'
    ;;
  promote)
    IDEA_ID="${2:?usage: idea-sweep.sh promote <idea:slug> <graph-ref> [by]}"
    GRAPH_REF="${3:?promote requires the intel-graph ref (e.g. doc:native-apple-app-proposal)}"
    BY="${4:-coding-session}"
    require_idea_id "${IDEA_ID}"
    cypher "MATCH (n:GrowthHypothesis {id: \"$(cypher_str "${IDEA_ID}")\"}) SET n.idea_status = \"promoted\", n.graph_ref = \"$(cypher_str "${GRAPH_REF}")\", n.idea_updated_at = localDateTime(), n.idea_updated_by = \"$(cypher_str "${BY}")\" RETURN n.id;" >/dev/null
    show_idea "${IDEA_ID}"
    ;;
  decline)
    IDEA_ID="${2:?usage: idea-sweep.sh decline <idea:slug> <reason> [by]}"
    REASON="${3:?decline requires a reason — the operator must hear why, not silence}"
    BY="${4:-coding-session}"
    require_idea_id "${IDEA_ID}"
    cypher "MATCH (n:GrowthHypothesis {id: \"$(cypher_str "${IDEA_ID}")\"}) SET n.idea_status = \"declined\", n.idea_status_reason = \"$(cypher_str "${REASON}")\", n.idea_updated_at = localDateTime(), n.idea_updated_by = \"$(cypher_str "${BY}")\" RETURN n.id;" >/dev/null
    show_idea "${IDEA_ID}"
    ;;
  ship)
    IDEA_ID="${2:?usage: idea-sweep.sh ship <idea:slug> [note] [by]}"
    NOTE="${3:-}"
    BY="${4:-coding-session}"
    require_idea_id "${IDEA_ID}"
    cypher "MATCH (n:GrowthHypothesis {id: \"$(cypher_str "${IDEA_ID}")\"}) SET n.idea_status = \"shipped\", n.idea_status_reason = \"$(cypher_str "${NOTE}")\", n.idea_updated_at = localDateTime(), n.idea_updated_by = \"$(cypher_str "${BY}")\" RETURN n.id;" >/dev/null
    show_idea "${IDEA_ID}"
    echo "ℹ shipped via direct cypher — no LifeGraphChange push fires (transitional gap); Aria's digest covers operator delivery."
    ;;
  *)
    echo "usage: idea-sweep.sh [pending|all|promote|decline|ship] ..." >&2
    exit 1
    ;;
esac
