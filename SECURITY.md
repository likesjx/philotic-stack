# Security Policy

## Reporting a vulnerability

Please report security issues through
**[GitHub private vulnerability reporting](https://github.com/likesjx/philotic-stack/security/advisories/new)**
rather than opening a public issue.

Include what you can: what the issue is, how to reproduce it, and what an
attacker could achieve. A proof of concept helps but is not required.

This is a personal project maintained by one person, so please set expectations
accordingly — there is no on-call rotation and no guaranteed response window.
Reports will be acknowledged and addressed on a best-effort basis.

## Supported versions

Only the current `main` branch is supported. There are no backported security
fixes for older tags.

## Threat model — read before deploying

The stack was built for a single operator running a small fleet of personal
machines on a private network. **It is not currently hardened for
multi-tenancy or for exposure to untrusted networks.** Several defaults reflect
that assumption:

- **Agents execute tools, including a shell.** `bash.exec` exists as a
  last-resort tool and requires per-call operator approval by default. If you
  grant an agent shell access, you are granting it to whatever the model
  decides to do. Toolset profiles and approval policies are the control here —
  configure them deliberately.
- **Inter-node mesh traffic uses an optional HMAC pre-shared key.** If you run
  more than one node, set it. Without it, beacon traffic on the mesh port is
  unauthenticated.
- **Local services bind loopback by default.** The graph server binds loopback
  unless `PHILOTIC_GRAPH_BIND` is set; if you expose it, also set
  `PHILOTIC_GRAPH_TOKEN` so writes and MCP require bearer auth. The same
  applies to any other service you rebind — assume no network-level auth
  unless you configured it.
- **Secrets live in a local vault** encrypted with a master key held in the OS
  keychain or a key file. A stolen unlocked machine is a full compromise of
  every credential the fleet holds, including provider API keys.
- **Agents hold model-provider credentials** and can reach the network through
  a governed egress path. Treat a compromised agent as a compromised key.

If you intend to run this somewhere less forgiving than a personal tailnet,
please open a discussion first — the multi-tenant story does not exist yet, and
that is a design gap rather than an oversight.

## Scope

In scope: privilege escalation between agents or roles, authentication or
authorization bypass on any exposed surface, secret disclosure, and remote code
execution paths that do not require pre-existing operator access.

Out of scope: anything requiring an already-compromised host, the documented
behaviour of `bash.exec` when an operator has deliberately granted and approved
it, and denial of service by an operator against their own node.
