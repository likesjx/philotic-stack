"""Unit tests for the schema handling in scripts/provision-agent-frontdoor.py (F5)."""

import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
_spec = importlib.util.spec_from_file_location("provision_agent_frontdoor", SCRIPTS / "provision-agent-frontdoor.py")
pf = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pf)

RECALL_SCHEMA = {"type": "object", "properties": {"context": {"type": "array"}}, "required": ["context"]}


def upstreams_reply():
    return {
        "mcp_upstreams": [
            {
                "config": {"upstream_id": "muninn-cortex"},
                "catalog": {
                    "upstream_id": "muninn-cortex",
                    "tools": [
                        {"remote_name": "muninn_recall", "description": "Recall memories.", "input_schema": RECALL_SCHEMA},
                        {"remote_name": "bogus", "description": "x", "input_schema": {"type": "string"}},
                    ],
                },
            },
            {"config": {"upstream_id": "no-catalog-yet"}},
        ]
    }


class SchemaTest(unittest.TestCase):
    def test_catalog_schemas_keeps_object_schemas_only(self):
        schemas = pf.catalog_schemas(upstreams_reply())
        self.assertEqual(set(schemas), {"muninn_recall"})
        self.assertEqual(schemas["muninn_recall"]["input_schema"], RECALL_SCHEMA)

    def test_build_tools_uses_known_schema_and_falls_back(self):
        tools = {t["name"]: t for t in pf.build_tools([], pf.catalog_schemas(upstreams_reply()))}
        self.assertEqual(tools["muninn_recall"]["input_schema"], RECALL_SCHEMA)
        self.assertTrue(tools["muninn_recall"]["description"].endswith("Recall memories."))
        self.assertEqual(tools["muninn_remember"]["input_schema"], pf.PASS_THROUGH_SCHEMA)
        self.assertEqual(len(tools), len(pf.MUNINN_TOOLS) + len(pf.GRAPH_TOOLS))

    def test_remote_description_is_capped(self):
        long = {"description": "y" * 5000, "input_schema": RECALL_SCHEMA}
        desc = pf.tool_description("Curated.", long)
        self.assertTrue(desc.startswith("Curated.\n\n"))
        self.assertLessEqual(len(desc), len("Curated.\n\n") + pf.MAX_REMOTE_DESCRIPTION + 1)

    def test_schema_file_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "schemas.json"
            path.write_text(json.dumps({"graph_status": {"description": "", "input_schema": {"type": "object"}}, "junk": 3}))
            schemas = pf.load_schema_file(str(path))
        self.assertEqual(set(schemas), {"graph_status"})

    def test_preapproval_rule_per_tool(self):
        config = pf.build_config([], 1, {})
        self.assertEqual(len(config["preapproval_rules"]), len(config["tools"]))


if __name__ == "__main__":
    unittest.main()
