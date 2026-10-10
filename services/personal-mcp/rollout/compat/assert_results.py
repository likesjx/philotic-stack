"""Assert real two-source fixture outputs without provider calls or private data."""
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
def read(name):
    return json.loads((root / (name + '.json')).read_text())

for kind in ['baseline', 'candidate']:
    short = read(kind + '-short-old-provider')
    assert short['identity'] and short['rules'], 'mandatory identity/rules lost'
    assert short['current_message_occurrences'] >= 1
    payload = read(kind + '-long')
    checkpoint = payload['checkpoint']
    assert checkpoint['session_id'] == 'synthetic-rollout'
    assert checkpoint['fallback_override'] == ['model.openrouter', 'model.ollama']
    assert checkpoint['active_turn']['recalled_memories'][0]['content'] == 'SYNTHETIC_UNATTESTED_RECALL'

candidate = read('candidate-long-old-provider')
baseline = read('baseline-long-old-provider')
assert not candidate['unattested_recall'] and not candidate['agent_graph'], 'privacy omission regressed'
assert baseline['unattested_recall'] and baseline['agent_graph'], 'baseline fixture failed to expose behavioral delta'
assert candidate['result_tail'] and not baseline['result_tail'], 'result truncation delta not exercised'
assert candidate['bytes'] > 12000, 'old provider budget gap was hidden'
assert candidate['current_message_occurrences'] == 2, 'expected old flat-prompt duplication changed'

for name in ['candidate-restores-baseline', 'baseline-restores-candidate']:
    checkpoint = read(name)['checkpoint']
    assert checkpoint['session_id'] == 'synthetic-rollout'
    assert checkpoint['fallback_override'] == ['model.openrouter', 'model.ollama']
    assert checkpoint['active_turn']['turn_id'] == 'synthetic-turn'
    assert checkpoint['active_turn']['recalled_memories'][0]['content'] == 'SYNTHETIC_UNATTESTED_RECALL'

print(json.dumps({'schema':1,'source_pair_fixture':'passed', 'provider_network_calls':0,
                  'implicit_recall':'fail_closed_omission', 'old_provider_budget':'not_enforced',
                  'release_authorized':False}, sort_keys=True))
