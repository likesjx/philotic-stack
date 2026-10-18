"""Synthetic-only observe checks. Importable driver; no production discovery.

The approved isolated launcher must supply a fresh backend and test-only admin
session. This file does not launch services, read credentials or install units.
"""
import hashlib
import datetime
import json
import re
import time
import urllib.error
import urllib.request

EXPECTED_SHA = '6a862d2fa39d48fe20b849b6ced66f2673822d1677e8bfe52300f8c16dc032d4'
A, B = 'exact_observe_a', 'exact_observe_b'
MARK_A, MARK_B = 'SYNTHETIC_AMBER_OBSERVE_7381', 'SYNTHETIC_COBALT_PRIVATE_9426'


def verify_binary(path):
    with open(path, 'rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    if digest != EXPECTED_SHA:
        raise ValueError('exact executable hash mismatch')
    return digest


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise RuntimeError('redirect refused')


class Client:
    """Fixed loopback endpoints; no proxies, retries, environment or logs."""
    def __init__(self, admin_opener, deadline):
        self.admin = admin_opener
        self.reader = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect())
        self.deadline = deadline

    def request(self, method, path, body=None, token=None, admin=False, mcp=False):
        if admin and token:
            raise ValueError('mixed authority')
        if not path.startswith('/api/') and not (mcp and path.startswith('/mcp')):
            raise ValueError('unexpected path')
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError('fixture deadline')
        headers = {'Content-Type': 'application/json'}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        request = urllib.request.Request(
            'http://127.0.0.1:' + ('18475' if mcp else '18476') + path,
            method=method, headers=headers,
            data=None if body is None else json.dumps(body).encode())
        try:
            response = (self.admin if admin else self.reader).open(request, timeout=min(5, remaining))
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(65537)
            if len(raw) > 65536:
                raise ValueError('response limit')
            # Error bodies are neither printed nor persisted.
            if response.status not in (200, 201):
                return response.status, None
            return response.status, json.loads(raw)

    def rpc(self, token, tool, arguments, query=''):
        return self.request('POST', '/mcp' + query,
            {'jsonrpc': '2.0', 'id': 17, 'method': 'tools/call',
             'params': {'name': tool, 'arguments': arguments}}, token=token, mcp=True)


def rpc_ok(reply):
    status, body = reply
    return (status == 200 and isinstance(body, dict) and body.get('jsonrpc') == '2.0'
            and body.get('id') == 17 and not body.get('error')
            and isinstance(body.get('result'), dict) and not body['result'].get('isError'))


def denied(reply):
    status, body = reply
    # A timeout, 5xx, malformed reply or empty result is NOT denial evidence.
    if status in (401, 403, 404):
        return True
    if status == 200 and isinstance(body, dict) and body.get('id') == 17:
        error = body.get('error', {})
        # Generic tool/storage failures are not successful access checks.
        message = str(error.get('message', ''))
        return (error.get('code') in (-32000, -32001, -32602) and
                bool(re.search(r'not found|not authorized|not allowed|forbidden|read.only|vault.*match|vault.*mismatch|observe', message, re.I)))
    return False


def run(client, verified_sha, expires, gateway_check=None, expiry_check=None):
    if verified_sha != EXPECTED_SHA:
        raise ValueError('binary verification required')
    receipt = {'binary_sha256': verified_sha, 'synthetic_only': True, 'cases': {}}
    keys = []
    phase = 'fresh_vault_inventory'

    def record(name, passed):
        receipt['cases'][name] = 'pass' if passed else 'fail'

    def must(reply, statuses=(200,)):
        status, body = reply
        if status not in statuses or not isinstance(body, dict):
            receipt['setup_http_status'] = status
            receipt['setup_body_type'] = type(body).__name__
            raise RuntimeError('synthetic setup failed')
        return body

    try:
        # Refuse nonempty or previously provisioned fixtures; never reuse stores.
        status, names = client.request('GET', '/api/vaults', admin=True)
        if status != 200 or names != ['default']:
            receipt['setup_http_status'] = status
            receipt['setup_body_type'] = type(names).__name__
            raise RuntimeError('fresh fixture required')
        phase = 'fresh_default_stats'
        stats = must(client.request('GET', '/api/stats?vault=default', admin=True))
        if stats.get('engram_count') != 0:
            raise RuntimeError('fresh fixture required')
        phase = 'private_vault_configuration'
        for vault in ('default', A, B):
            # Do not disable reinforcement: observe suppression must be exercised.
            must(client.request('PUT', '/api/admin/vaults/config',
                                {'name': vault, 'public': False}, admin=True))
        phase = 'synthetic_record_creation'
        ids = {}
        for vault, marker in ((A, MARK_A), (B, MARK_B)):
            item = must(client.request('POST', '/api/engrams?vault=' + vault,
                {'vault': vault, 'concept': marker, 'content': marker + ' synthetic recall fixture.'},
                admin=True), (200, 201))
            ids[vault] = item['id']
        phase = 'synthetic_record_inventory'
        inventory_path = '/api/engrams?vault=' + A + '&limit=10&offset=0'
        baseline = must(client.request('GET', inventory_path, admin=True))
        baseline_ids = [item['id'] for item in baseline.get('engrams', [])]
        if baseline_ids != [ids[A]]: raise RuntimeError('unexpected synthetic inventory')
        baseline_stats = must(client.request('GET', '/api/stats?vault=' + A, admin=True))
        receipt['synthetic_count_before'] = baseline_stats.get('engram_count')
        phase = 'observe_key_creation'
        key = must(client.request('POST', '/api/admin/keys',
            {'vault': A, 'label': 'disposable-exact-binary-check', 'mode': 'observe', 'expires': expires},
            admin=True), (201,))
        phase = 'observe_key_metadata'
        token = key['token']
        keys.append(key['key']['id'])
        try:
            actual_expiry = datetime.datetime.fromisoformat(key['key']['expires_at'].replace('Z', '+00:00'))
            requested_expiry = datetime.datetime.fromisoformat(expires.replace('Z', '+00:00'))
            ttl = (actual_expiry - datetime.datetime.now(datetime.timezone.utc)).total_seconds()
            if not (0 < ttl <= 120 and abs((actual_expiry-requested_expiry).total_seconds()) < 1):
                raise ValueError('expiry outside fixture bound')
            record('test_key_expiry_metadata_bounded', True)
        except (KeyError, ValueError, TypeError):
            record('test_key_expiry_metadata_bounded', False)
            raise RuntimeError('test key expiry not established')
        phase = 'observe_acceptance'
        args = {'vault': A, 'context': [MARK_A], 'threshold': 0, 'limit': 10, 'read_only': True}
        read_args = {'vault': A, 'id': ids[A], 'read_only': True}
        def negative(name, passed):
            # A denied expired key proves nothing about vault/write enforcement.
            # Require a successful same-key own-vault control AFTER each denial.
            control = client.rpc(token, 'muninn_read', read_args)
            active = (datetime.datetime.now(datetime.timezone.utc) < actual_expiry
                      and rpc_ok(control) and MARK_A in json.dumps(control[1]))
            if not active:
                receipt['cases'][name] = 'inconclusive: same-key positive control failed or expired'
                raise RuntimeError('denial not attributable to scope')
            record(name, passed)
        before = client.rpc(token, 'muninn_read', read_args)
        # Writes enqueue asynchronous FTS indexing. Poll only a read-only
        # positive control, before any negative test, for at most five seconds.
        ready_end = time.monotonic() + 5
        while True:
            recall = client.rpc(token, 'muninn_recall', args)
            if rpc_ok(recall) and MARK_A in json.dumps(recall[1]): break
            if time.monotonic() >= ready_end: break
            time.sleep(.1)
        record('mcp_observe_own_vault_recall', rpc_ok(recall) and MARK_A in json.dumps(recall[1])
               and MARK_B not in json.dumps(recall[1]))
        negative('mcp_body_cross_vault_denied', denied(client.rpc(token, 'muninn_recall',
               dict(args, vault=B, context=[MARK_B]))))
        query_reply = client.rpc(token, 'muninn_recall', args, '?vault=' + B)
        # Ignoring a query parameter while enforcing the key's vault is isolation,
        # but not explicit rejection. Report those outcomes separately.
        negative('mcp_query_cross_vault_explicit_denial', denied(query_reply))
        negative('mcp_query_no_foreign_content', denied(query_reply) or
               (rpc_ok(query_reply) and MARK_A in json.dumps(query_reply[1]) and MARK_B not in json.dumps(query_reply[1])))
        negative('mcp_direct_foreign_id_denied', denied(client.rpc(token, 'muninn_read',
               {'vault': A, 'id': ids[B], 'read_only': True})))
        negative('mcp_write_rejected', denied(client.rpc(token, 'muninn_remember',
               {'vault': A, 'concept': 'DENIAL_ONLY', 'content': 'SYNTHETIC_DENIED_WRITE'})))
        negative('mcp_delete_rejected', denied(client.rpc(token, 'muninn_forget',
               {'vault': A, 'id': ids[A]})))
        negative('rest_query_cross_vault_denied', denied(client.request('GET',
               '/api/engrams/' + ids[B] + '?vault=' + B, token=token)))
        status, _ = client.request('POST', '/api/activate',
               {'vault': B, 'context': [MARK_B]}, token=token)
        negative('rest_body_cross_vault_denied', status in (400, 401, 403))
        negative('rest_direct_foreign_id_denied', denied(client.request('GET',
               '/api/engrams/' + ids[B] + '?vault=' + A, token=token)))
        status, _ = client.request('POST', '/api/engrams',
               {'vault': A, 'concept': 'DENIAL_ONLY', 'content': 'SYNTHETIC_DENIED_WRITE'}, token=token)
        negative('rest_write_rejected', status == 403)
        after = client.rpc(token, 'muninn_read', read_args)
        record('observed_record_response_unchanged', rpc_ok(before) and rpc_ok(after) and before == after)
        count = must(client.request('GET', '/api/stats?vault=' + A, admin=True))
        receipt['synthetic_count_after'] = count.get('engram_count')
        after_inventory = must(client.request('GET', inventory_path, admin=True))
        after_ids = [item['id'] for item in after_inventory.get('engrams', [])]
        record('denied_writes_did_not_add_records', after_ids == baseline_ids == [ids[A]])
        receipt['cases']['all_cognitive_storage_writes_suppressed'] = 'unsupported: response equality does not cover activation logs, feedback or association storage'
        receipt['cases']['actual_gateway_adapter'] = (
            gateway_check(token, A, MARK_A) if gateway_check else 'not_run: gateway callback absent')
        receipt['cases']['expired_key_rejected'] = (
            expiry_check(token, actual_expiry, args) if expiry_check else 'not_run: expiry callback absent')
    except Exception as error:
        receipt['failure_phase'] = phase
        receipt['failure_type'] = type(error).__name__
        receipt['setup_or_transport'] = 'failed_or_uncertain; no retry'
    finally:
        receipt['test_keys_revoked'] = True
        for key_id in keys:
            try:
                status, _ = client.request('DELETE', '/api/admin/keys/' + key_id + '?vault=' + A, admin=True)
                if status not in (200, 204): receipt['test_keys_revoked'] = False
            except Exception:
                receipt['test_keys_revoked'] = False
        receipt['requires_launcher_shutdown'] = True
    return receipt
