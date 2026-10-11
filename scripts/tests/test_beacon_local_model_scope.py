"""Public source configuration contract; does not execute or validate Rust."""
import pathlib
import unittest
import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]

class BeaconLocalModelScope(unittest.TestCase):
    def test_inventory_bound_fallback_and_source_order_are_preserved(self):
        config = yaml.safe_load((ROOT / 'ansible/host_vars/jane-vps.yml').read_text())
        agents = next(value for value in config.values() if isinstance(value, dict) and 'beacon' in value)
        role = agents['beacon']['turn_loop_config']
        self.assertEqual(role['fallback_tiers'], ['model.openrouter', 'model.ollama'])
        self.assertEqual(role['model_bindings'], {'model.ollama': 'qwen2.5:1.5b'})

if __name__ == '__main__':
    unittest.main()
