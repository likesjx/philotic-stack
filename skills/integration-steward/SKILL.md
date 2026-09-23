---
name: integration-steward
description: Use this skill whenever a philote must connect to an external HTTP API (fitness trackers, calendars, SaaS APIs, webhooks) on the operator's behalf. It walks the governed path — read the real API contract, bind the narrowest surface, get the credential provisioned outside the model, prove one call, then automate — and it names the failure modes so the agent triages instead of guessing.
catalog:
  skill_name: integration.steward
  implied_tools:
    - integration.list
    - integration.bind_http
    - integration.unbind
    - session.status
    - cron.list
    - cron.register
  validation_state: validated
  skill_markers:
    - governed
    - egress
    - high_agency
  field_sources:
    required_fields:
      - binding_id
      - base_url
      - allowed_methods
      - allowed_path_prefixes
    optional_fields:
      - credential_header
      - credential_format
      - placement
      - traffic_class
      - grant_agents
    repo_skill_path: skills/integration-steward/SKILL.md
    workflow: "integration.list → read the API contract → integration.bind_http (narrow, local, credential declared) → operator runs phil integration set-credential → smoke one http:<binding>.request → automate with cron → record"
---

# Integration Steward

Use this skill when the operator wants you to talk to an external API. You never get raw network access and you never see secrets; you get a **binding** that projects one governed tool, `http:<binding_id>.request`, and the hotel's egress runner does the I/O.

## The ladder — do these in order, do not skip

1. **Audit first.** `integration.list`. If a binding for this API already exists (yours or another agent's), re-use or re-bind it under the same `binding_id`. Never create a second binding for the same host.
2. **Read the contract before binding.** Establish from the vendor's documentation, not from memory: base URL, the exact paths you need, the auth header name and format, and whether the API pushes (webhooks) or must be polled. If you cannot cite the path, you do not know it.
3. **Bind the narrowest surface.** `integration.bind_http` with:
   - `allowed_path_prefixes` = only the paths you will actually call (e.g. `/v1/workouts`), never a whole API.
   - `allowed_methods` = only what those paths need. Start read-only (`GET`).
   - `credential_header` + `credential_format` when the API needs a key (e.g. `api-key` / `{}`, or `Authorization` / `Bearer {}`). Declaring it tells the runner to inject the secret; you never see the value.
   - `placement: {mode:"local"}` unless the operator has told you this hotel must exit elsewhere. Only prefer a remote exit hotel that `integration.list` reports as reachable.
   - `requires_approval: false` only for read-only surfaces the operator has already asked for; keep it true for writes.
4. **Get the credential provisioned — by the operator, outside the model.** The tool result will say so. Tell the operator exactly what to run and stop there:

   ```
   printf '%s' '<API KEY>' > /tmp/hevy.key
   phil integration set-credential <binding_id> --owner <your agent id> --credential-file /tmp/hevy.key
   rm /tmp/hevy.key
   ```

   Never ask the operator to paste the key into chat.
5. **Smoke one call and read the status code.** `http:<binding_id>.request` with the smallest read (e.g. `GET /v1/workouts?page=1&pageSize=1`). Interpret honestly:

   | Result | Meaning | Your move |
   |---|---|---|
   | `200` | live | say so, quote one field from the body |
   | `401` / `403` | path exists, credential missing or wrong | step 4 not done, or wrong header/format → re-bind |
   | `404` | wrong path prefix | re-read the contract, re-bind |
   | `429` | rate limited | back off; lower cron frequency |
   | timeout / no reply after ~30 s | the runner never ran (dormant runner, unreachable exit hotel) | `integration.list` → check `placement` and `execution_node`; re-bind with `placement:local` |

   Until the smoke returns `2xx`, the integration is **not live**. Do not tell the operator it is.
6. **Automate only after the smoke is green.** For ingestion, prefer polling with `cron.register` (e.g. every 15 min `GET /v1/workouts/events?since=<last>`) and write results with `life.observe`. Keep the cron payload self-contained: which tool, which path, what to do with the result, what to do on `401`/`404`/timeout (report once, do not retry blindly).
7. **Record.** In your reply: binding id, paths, credential header (name only), placement, smoke status code, and the cron id if any.

## Webhooks

A vendor webhook needs a public HTTPS URL that the stack can receive on. The stack has **no inbound webhook ingress today**. An outbound binding to a vendor's `/webhook-subscription` endpoint does not create one, and an MCP endpoint (`mcp.provision`) is a JSON-RPC surface for MCP clients, not a webhook receiver. If push delivery is required, say that it needs the philotic-web webhook ingress (not yet built) and offer polling instead.

## Triage rules when it fails

- A hung request is a **runner/placement** problem, not Tailscale, not memory, not the operator's network. Check `integration.list` first.
- `401` is a **credential** problem. Ask whether step 4 ran; do not rebind blindly.
- `404` is **your** path. Re-read the contract.
- Memory-backend errors (Muninn 401) are unrelated to integrations; report them separately.
- Never invent a cause. If the evidence does not point to one, say "unknown; here is the status code and the binding" and ask.

## Anti-patterns

- Announcing "the integration is live" after a successful bind. A bind is a permission grant, not a connection.
- Binding `/` or a broad prefix "to be safe".
- Preferring a remote exit hotel that has never been proven reachable for this binding.
- Trying to connect to the operator's own MCP frontdoor as if it were the vendor.
- Asking for the API key in chat.
- Reporting "connection issues" for a failure you did not diagnose.
