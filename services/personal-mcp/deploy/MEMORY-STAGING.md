# Inert whole-vault package staging

The existing dedicated `ansible/deploy_personal_mcp.yml` play can select
`percival_profile=memory-stage-v1`. The default remains `synthetic`, and all
profiles remain disabled until `percival_enabled=true` is explicit. The original
synthetic rollout and its handlers are unchanged.

This new profile only stages files. It cannot activate or disable a gateway.
`percival_memory_operation=activate` fails before inspecting or changing staging
files. There is no reviewed installed activation contract yet: installed gateway
configuration, broker policy compatibility, Node qualification and hotel binary
provenance must be reconciled separately. Staging grants no access and creates
no admission, consent, credentials, broker policy, accounts or service wiring.

## Exact inputs and output

The profile pins the 16 gateway modules from
`709c0d7e3de6e85df6ba45786fbd7537123e4011` and the previously qualified Linux Node
binary SHA-256
`7fde7b8afa198da66257f42ee2001d874c7355631e6d1579a5fb5ef1f246df4c`.
It verifies every source and destination hash. It does not build or execute Node.
The source gateway/broker pin is not a claim about the installed hotel binary.

Explicit inputs are:

- `percival_enabled: true`, `percival_profile: memory-stage-v1` and
  `percival_memory_operation: stage`.
- `percival_memory_package_dir`: absolute controller directory containing the
  verified package's `gateway/` and `bin/node` entries.
- `percival_memory_config_file`: absolute controller path to the approved public
  JSON configuration, and `percival_memory_config_sha256`: its exact byte digest.

The closed public configuration schema is defined in `tasks/memory_stage.yml`.
It allows only default-vault recall, the canonical issuer, the existing broker
socket and the explicit whole-vault mapping. Both client policies are present
because that is the gateway contract; independent issuer admission must still
keep ChatGPT disabled. Unknown fields and inline credentials fail validation.
Configuration authorization is separate from staging these inert bytes.

The destination is
`/opt/percival-personal-mcp/staged/memory-v1-<configuration-sha256>/`, containing
`gateway/`, `bin/node` and `gateway.json`. This fixed profile identifies one exact
runtime artifact; its configuration digest distinguishes configuration variants.
There is no change to `current`, `/etc`, units, accounts or services, and no
handler notification. The approved production command must use this dedicated
play and reviewed public variables; staging itself is still a production file
mutation requiring its own release approval.

A complete identical existing stage is verified without modification. Existing
missing, extra, symlinked, changed or incorrectly owned/mode entries are refused,
not repaired. An interrupted first stage can leave an inert partial directory;
subsequent attempts refuse it. Cleanup/replacement requires explicit operator
review and is not automated. Staging does not supply production rollback.

If installed metadata proves the exact module closure is already present and its
Node runtime is qualified, a smaller eventual release may reuse that installation
and adopt only reviewed configuration. This staging option does not require
replacing an already suitable runtime or upgrading the hotel.

## Disposable validation

`node --test memory-stage.test.mjs install-contract.test.mjs` verifies exact
module hashes, constructs the actual whole-vault gateway with synthetic issuer
and upstream objects without opening a listener, rejects a missing dynamic
module, checks the staging mutation surface and preserves the historical
synthetic contract tests.

The real role execution fixture requires Ansible, root inside a disposable Linux
container, and the exact qualified Node input:

```sh
PERCIVAL_ISOLATED_LINUX_FIXTURE=1 python3 -B \
  services/personal-mcp/scoped-vault-tests/memory_stage_acceptance.py \
  --node /__e/node24/bin/node
```

It executes 13 real `ansible-playbook` cases covering disabled/unknown/activation
refusal, missing modules, source/configuration drift, scope expansion, exact
stage, unchanged replay, destination drift, unexpected files and symlinks.
Fixture roots are restricted to explicit local connections and
`/tmp/muninn-memory-stage-*`; there is no remote fixture mode. Surrounding live
surfaces are represented by disposable sentinels. Tests neither call systemd nor
inspect production state. Source tests alone do not establish installed service
or broker compatibility.
