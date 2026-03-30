# Generated Architecture Documents

**WARNING**: All files in this directory are AUTO-GENERATED from the graph intelligence database.

Do not edit these files manually. Any changes will be overwritten on the next generation.

## Files

| File | Source | Trigger |
|------|--------|---------|
| `SEAM_REGISTRY.md` | All `seam` nodes + `applies_to` edges | On seam node creation/update |
| `ROADMAP.md` | Seams with `depends_on` edges | Manual or periodic regeneration |
| `ARCH_RULES.md` | Rules extracted from accepted/implemented proposals | On proposal disposition change |
| `ARCHITECTURE_STATUS.md` | Aggregate proposal statuses | Periodic or on significant changes |

## Generation Process

1. Graph mutations occur via MCP tools (`graph_update_node`, `graph_create_edge`)
2. Change events broadcast via WebSocket
3. Optional: `graph_writeback` tool serializes graph state to markdown
4. Generated files updated with new frontmatter + content
5. Auto-commit with provenance: `"graph: regenerate <FILE> from graph state"`

## Manual Regeneration

If you need to force regeneration:

```bash
# Via MCP tool
curl -X POST http://localhost:8901/mcp \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_writeback",
      "arguments": {
        "node_id": "doc:seam-registry",
        "commit": true
      }
    }
  }'
```

Or trigger full rescan:
```bash
curl -X POST http://localhost:8900/api/scan
```

## Source of Truth

**The graph is canonical.** These markdown files are human-readable projections.

If graph and files disagree:
- Graph wins
- Run `graph_writeback` to sync
- Files are secondary

See `../GRAPH_AS_SOURCE_OF_TRUTH.md` for full architecture.
