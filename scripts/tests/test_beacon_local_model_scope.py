"""Public source configuration contract; does not execute or validate Rust."""
import pathlib
import unittest
import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]

class BeaconLocalModelScope(unittest.TestCase):
    def test_unverified_qwen_is_not_wired_and_source_order_is_preserved(self):
        config = yaml.safe_load((ROOT / 'ansible/host_vars/jane-vps.yml').read_text())
        agents = next(value for value in config.values() if isinstance(value, dict) and 'beacon' in value)
        role = agents['beacon']['turn_loop_config']
        self.assertEqual(role['fallback_tiers'], ['model.openrouter', 'model.ollama'])
        self.assertNotIn('model.ollama', role.get('model_bindings', {}))

if __name__ == '__main__':
    unittest.main()
