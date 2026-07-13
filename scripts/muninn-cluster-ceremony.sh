#!/usr/bin/env bash
# Muninn cluster ceremony — unified replicated memory across the fleet.
#
# Topology (decided 2026-07-12, muninn 01KXC99SVSPE46AAQ28M3TGEW0):
#   primary : vps-jane   (only always-on node)
#   replicas: this Mac (local-bjork) + mbp-jane, cortex-addr -> vps
#   traffic : tailnet only (bind Tailscale IPs)
# Per-agent self_agent-* vaults keep separation; intentional merges:
#   user_likesjx x3, default x3, and Beacon's mbp/vps split brain.
#
# Diagnostic ground truth (2026-07-13, all verified live on v0.7.0):
# - "Unexportable" vaults are EMPTY (0 engrams) — export 404s on them by
#   design (registry written on first engram). Skip empties; nothing lost.
# - Exports require admin auth: -u root -p'<password>' (mesh-config
#   context_graph.muninn creds work on the Air and mbp-jane).
# - vps admin password is CUSTOM: recover via `ansible-vault view
#   ansible/vault/jane-vps.yml` in the MAIN checkout, or rotate by
#   restarting the vps daemon with MUNINN_ADMIN_PASSWORD=<new>.
# - `muninn cluster enable` CLI ALWAYS 401s (sends no session cookie).
#   Use REST: login -> POST /api/admin/cluster/enable with cookie.
# - Consider upgrading all three daemons to v0.8.0 BEFORE clustering.
#
# This script is a GUIDED runbook: every mutating step prompts. Run it
# from a session where the operator is present. Steps are idempotent-ish
# but read each prompt.
set -euo pipefail

confirm() { read -r -p "==> $1 [y/N] " a; [[ "$a" == "y" ]] || { echo "aborted"; exit 1; }; }

STAMP=$(date +%Y%m%d-%H%M%S)
AIR_TS=100.64.230.106
MBP_TS=100.79.239.64
VPS_TS=100.64.212.8
CLUSTER_PORT=8490   # pick an unused tailnet port for cluster traffic

echo "Muninn cluster ceremony — $STAMP"
echo "You will need: local/mbp admin password (mesh-config context_graph.muninn),"
echo "vps admin password (ansible-vault view ansible/vault/jane-vps.yml), and a"
echo "generated cluster secret (openssl rand -hex 32) stored in the hotel vault."
echo

# ── Phase A: offline backups (one host at a time; brief memory outage) ──────
confirm "Phase A: stop LOCAL muninn daemon and take offline backup?"
pkill -f "muninn --daemon" || true; sleep 2
muninn backup --output ~/muninn-backups/local-$STAMP
(cd ~/code/muninndb && just start) || echo "!! restart local muninn manually (just start in ~/code/muninndb)"
sleep 3; muninn status

confirm "Phase A: mbp-jane offline backup (stops its daemon briefly)?"
ssh mbp-jane 'pkill -f "muninn --daemon" || true; sleep 2; muninn backup --output ~/muninn-backups/mbp-'"$STAMP"'; (cd ~/code/muninndb && just start) || echo "!! restart mbp muninn manually"; sleep 3; muninn status'

confirm "Phase A: vps-jane offline backup (stops its daemon briefly)?"
ssh deploy@jane-vps 'pkill -f "muninn" || true; sleep 2; muninn backup --output ~/muninn-backups/vps-'"$STAMP"'; muninn start || sudo systemctl start muninn || echo "!! restart vps muninn manually"; sleep 3; muninn status'

# ── Phase B: fresh authenticated exports of every NON-EMPTY vault ───────────
echo "Phase B: export non-empty vaults with admin auth. For each host:"
echo '  muninn vault export --vault <name> --output <file> -u root -p"<pw>"'
echo "Local + mbp non-empty as of 2026-07-13: default, self_agent-bjork-01,"
echo "self_agent-coach (local); default, self_agent-aria, self_agent-jane,"
echo "self_agent-astrid (mbp). vps: check with vault list once auth works."
confirm "Exports done and centralized under ~/muninn-exports/ ?"

# ── Phase C: consolidate into the future primary (vps) ──────────────────────
echo "Phase C: copy exports to vps and import (muninn vault import) — import"
echo "collision-checks against the engram registry, so empty same-name vaults"
echo "on the target do not block. Merge order: vps keeps its own data; import"
echo "local+mbp vaults; for the shared ones (default, user_likesjx) import"
echo "into the SAME vault name (merge), Beacon's two vaults likewise."
confirm "Consolidation complete and spot-checked (muninn_recall on vps)?"

# ── Phase D: enable cluster — REST, not CLI ─────────────────────────────────
cat <<'EOF'
Phase D (vps primary — run ON the vps, replace <pw>/<secret>):
  curl -s -c /tmp/mj.jar -X POST http://127.0.0.1:8476/api/auth/login \
       -H 'Content-Type: application/json' -d '{"username":"root","password":"<pw>"}'
  curl -s -b /tmp/mj.jar -X POST http://127.0.0.1:8475/api/admin/cluster/enable \
       -H 'Content-Type: application/json' \
       -d '{"role":"primary","bind_addr":"100.64.212.8:8490","cluster_secret":"<secret>"}'
  rm /tmp/mj.jar

Then each replica (on the Air and mbp-jane, same login dance locally):
  ... -d '{"role":"replica","bind_addr":"<this-host-ts-ip>:8490",
           "cluster_secret":"<secret>","cortex_addr":"100.64.212.8:8490"}'

Verify: muninn cluster info && muninn cluster status  (on all three)
EOF
confirm "Cluster enabled on all three and replication lag near zero?"

# ── Phase E: verification ────────────────────────────────────────────────────
echo "Phase E checklist:"
echo "  - every hotel: aiua log shows fetch_memory_config green after restarting hotels (or wait for next poll)"
echo "  - muninn_where_left_off on the Air shows merged (vps-origin) view"
echo "  - each self_agent-* vault intact (spot muninn_recall per agent vault)"
echo "  - kill -STOP the vps daemon briefly -> replicas still serve reads"
echo "Record the outcome in muninn + MEMORY.md + intel-graph. Done."
