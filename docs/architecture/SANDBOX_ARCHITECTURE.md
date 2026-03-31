---
title: Sandbox Architecture
doc_type: architecture
domain: philotic-sandbox
status: active
last_updated: 2026-03-31
tags:
- sandbox
- security
- architecture
related_docs:
- ARCHITECTURE.md
- ../guides/SANDBOX_POLICIES.md
---

# Sandbox Architecture

## Overview

The `philotic-sandbox` crate provides a policy-driven sandbox runtime for executing
shell commands within the philotic-stack framework. It replaces direct
`std::process::Command` execution with a separate worker process that enforces
filesystem, network, and syscall restrictions.

## Architecture Diagram

```
┌───────────────────────────────────┐
│            Hotel (Ansible)        │
│                                   │
│  Manages guest lifecycle,         │
│  creates socket paths,            │
│  spawns workers                   │
└─────────┬───────────┬─────────────┘
          │           │
          │           │ spawn
          ▼           ▼
┌─────────────────┐  ┌──────────────────────────────────┐
│   Tool-Runner   │  │     Sandbox Worker               │
│   (Guest)       │  │     (philotic-sandbox-worker)     │
│                 │  │                                    │
│  Uses           │  │  1. Load policy (TOML)            │
│  SandboxedShell │  │  2. Bind UDS listener             │
│  Executor to    │──│  3. Apply Landlock rules           │
│  send requests  │  │  4. Apply seccomp filter           │
│  over UDS       │  │  5. Accept connections             │
│                 │  │  6. Validate + execute commands     │
└─────────────────┘  └──────────────────────────────────┘
        │                          │
        │    Unix Domain Socket    │
        │◄─────────────────────────│
        │  ExecuteCommandRequest   │
        │─────────────────────────►│
        │  ExecuteCommandResponse  │
        │◄─────────────────────────│
```

## How It Fits Into the Hotel/Guest Model

The philotic-stack uses a hotel/guest architecture:

- **Hotel** — the runtime host (Ansible daemon) that materializes guest processes
- **Guests** — specialized processes: agent-core, tool-runners, membranes
- **Tool-runners** — non-cognitive guests that execute tool calls

The sandbox worker is a **companion process** to the tool-runner. When the system
is configured for sandboxed execution, the tool-runner sends shell commands to the
sandbox worker over a Unix domain socket instead of executing them directly.

### Execution Modes

The `ShellExecutionMode` enum controls how commands are executed:

- **Direct** — `std::process::Command` (development, no OS-level restrictions)
- **Sandboxed** — IPC to the sandbox worker (production, OS-level enforcement)

Both modes implement the `ShellExecutor` trait, making the switch transparent.

## Security Layers

### Layer 1: Application-Level Policy

The TOML policy file defines:
- **Command allowlist/denylist** — which binaries can be executed
- **Filesystem paths** — read, write, and execute path patterns
- **Environment filtering** — only allowlisted env vars pass through
- **Timeout enforcement** — per-command and global maximums

This layer works on all platforms.

### Layer 2: Landlock (Linux 5.13+)

Kernel-level filesystem access control. The worker applies Landlock rules
**after** loading config but **before** accepting commands:

- Read-only paths get `ReadFile | ReadDir` access
- Write paths get `ReadFile | ReadDir | WriteFile | MakeDir | MakeReg`
- Execute paths get `ReadFile | ReadDir | Execute`

### Layer 3: Seccomp (Linux)

Syscall filtering via BPF. Four profiles are available:

| Profile | Use Case | Key Additions |
|---------|----------|---------------|
| `strict` | Computation only | Base syscalls only |
| `file_processor` | File I/O | + openat, stat, getcwd, etc. |
| `shell_executor` | Shell commands | + fork, exec, pipe, wait |
| `network_server` | Network access | + socket, connect, bind, etc. |

### Defense in Depth

OS-level restrictions (Landlock + seccomp) are applied to the worker process
itself. Even if the IPC protocol is exploited, the kernel enforces restrictions
that the compromised process cannot bypass.

## IPC Protocol

**Transport:** Unix domain socket (stream).

**Wire format:** Length-prefixed bincode frames:
```
┌──────────────────────┬──────────────────────────────┐
│ 4 bytes (LE u32)     │ bincode payload              │
│ payload length       │ (ExecuteCommandRequest or    │
│                      │  ExecuteCommandResponse)     │
└──────────────────────┴──────────────────────────────┘
```

**Flow:**
1. Client connects to UDS
2. Client sends `ExecuteCommandRequest` frame
3. Worker validates against policy
4. Worker executes command (fork+exec)
5. Worker sends `ExecuteCommandResponse` frame
6. Connection can be reused for multiple requests

## Startup Sequence

```
1. Parse CLI args (--policy, --socket, --strict)
2. Load + validate SandboxPolicy from TOML
3. Create UDS listener at socket path
4. Apply Landlock rules  ← OS restrictions applied HERE
5. Apply seccomp filter  ← before any commands run
6. Enter event loop      ← now sandboxed
```

## Graceful Degradation

On non-Linux or older kernels:
- Landlock unavailable → warning logged, application-level enforcement only
- Seccomp unavailable → warning logged, application-level enforcement only
- `--strict` flag → requires OS enforcement, fails if unavailable
