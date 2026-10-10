"""Assert real two-source fixture outputs without provider calls or private data."""
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
override = {'origin_tier_role':'model.openrouter', 'active_tier_role':'model.openrouter',
            'reason':'synthetic', 'since_epoch_ms':1, 'last_probe_epoch_ms':1, 'notice_sent':False}
limits = {'input_tokens':512, 'output_tokens':256}
def read(name):
    return json.loads((root / (name + '.json')).read_text())

for kind in ['baseline', 'candidate']:
    short = read(kind + '-short-old-provider')
    assert short['identity'] and short['rules'], 'mandatory identity/rules lost'
    assert short['current_message_occurrences'] >= 1
    for size in ['short', 'long']:
        request = read(kind + '-' + size + '-old-provider')
        assert request['loopback_provider_calls'] == 1 and request['external_provider_calls'] == 0
        assert request['serialized_prompt_matches'] and not request['context_budget_forwarded']
    payload = read(kind + '-long')
    checkpoint = payload['checkpoint']
    assert checkpoint['session_id'] == 'synthetic-rollout'
    assert checkpoint['fallback_override'] == override
    if kind == 'candidate':
        assert checkpoint['context_request_limits'] == limits
    else:
        assert 'context_request_limits' not in checkpoint, 'baseline gained unsupported context limits'
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
    assert checkpoint['fallback_override'] == override
    if name == 'candidate-restores-baseline':
        assert checkpoint['context_request_limits'] is None, 'legacy missing limit should restore as unset'
    else:
        assert 'context_request_limits' not in checkpoint, 'baseline unexpectedly retained unsupported limit'
    assert checkpoint['active_turn']['turn_id'] == 'synthetic-turn'
    assert checkpoint['active_turn']['recalled_memories'][0]['content'] == 'SYNTHETIC_UNATTESTED_RECALL'

print(json.dumps({'schema':1,'source_pair_fixture':'passed', 'external_provider_calls':0,
                  'loopback_provider_calls':4,
                  'implicit_recall':'fail_closed_omission', 'old_provider_budget':'not_enforced',
                  'context_limit_checkpoint':'legacy_missing_restores_unset_and_baseline_drops_candidate_field',
                  'release_authorized':False}, sort_keys=True))
