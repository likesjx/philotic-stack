---
title: "Writing Sandbox Policies"
doc_type: guide
domain: philotic-sandbox
status: active
last_updated: 2026-03-23
tags:
  - sandbox
  - policy
  - guide
related_docs:
  - ../architecture/SANDBOX_ARCHITECTURE.md
---

# Writing Sandbox Policies

This guide explains how to write custom sandbox policies for the philotic-stack
sandbox runtime.

## Policy File Format

Policies are TOML files with five sections:

```toml
[sandbox]      # Required: metadata and global limits
[commands]     # Command allowlist/denylist
[filesystem]   # Path-based access control
[environment]  # Environment variable filtering
[network]      # Network access mode
[seccomp]      # Syscall filtering profile
```

## Sections

### `[sandbox]` — Metadata and Limits

```toml
[sandbox]
name = "my-policy"              # Required. Identifies this policy.
version = "1.0"                 # Optional. Default: "1.0"
max_timeout_ms = 30000          # Max execution time per command. Default: 30000
max_concurrent = 4              # Max parallel executions. Default: 4
```

### `[commands]` — Command Control

```toml
[commands]
allowed = ["ls", "cat", "grep", "git"]
denied = ["sudo", "rm", "chmod"]
```

- **`allowed`**: Commands the sandbox can execute. Supports glob patterns (`*`).
- **`denied`**: Commands explicitly forbidden. Takes priority over `allowed`.
- A command not in `allowed` is implicitly denied.

**Glob example:**
```toml
allowed = ["cargo*", "rust*"]   # Matches cargo, cargo-fmt, rustc, rustfmt, etc.
```

### `[filesystem]` — Path Access Control

```toml
[filesystem]
read_paths = ["/tmp/work/**", "./workspace/**", "/usr/lib/**"]
write_paths = ["./workspace/output/**", "/tmp/scratch/**"]
execute_paths = ["/usr/bin/**", "/bin/**"]
```

- **`read_paths`**: Paths the sandbox can read from.
- **`write_paths`**: Paths the sandbox can write to (implies read access at OS level).
- **`execute_paths`**: Paths containing executables the sandbox can run.
- All patterns use glob syntax with `**` for recursive matching.
- The working directory (`cwd`) of each command is validated against `read_paths`.

**Tips:**
- Always include standard library paths (`/usr/lib/**`, `/lib/**`) for read access.
- Include `/etc/ssl/certs/**` if the sandbox needs TLS certificate verification.
- Use `./workspace/**` for relative workspace paths.

### `[environment]` — Environment Variables

```toml
[environment]
allowlist = ["PATH", "HOME", "RUST_LOG", "LANG"]
forced = { SANDBOX = "1", PHILOTIC_SANDBOXED = "true" }
```

- **`allowlist`**: Only these environment variables pass through from the caller.
- **`forced`**: These variables are always set, overriding any caller-provided values.
- Any variable not in `allowlist` is stripped from the child process environment.

### `[network]` — Network Access

```toml
[network]
mode = "none"                   # "none", "allowlist", or "unrestricted"
allowed_hosts = []              # Only used when mode = "allowlist"
```

| Mode | Description |
|------|-------------|
| `none` | No network access (default, recommended for most policies) |
| `allowlist` | Only listed host:port pairs allowed |
| `unrestricted` | No restrictions (dev only, never use in production) |

**Allowlist example:**
```toml
[network]
mode = "allowlist"
allowed_hosts = [
    "api.github.com:443",
    "crates.io:443",
]
```

### `[seccomp]` — Syscall Profile

```toml
[seccomp]
profile = "shell_executor"
```

| Profile | Use Case | Allows |
|---------|----------|--------|
| `strict` | Pure computation | Basic I/O, memory, signals |
| `file_processor` | File manipulation | + filesystem operations |
| `shell_executor` | Shell commands (default) | + fork, exec, pipe, wait |
| `network_server` | Network services | + socket, connect, bind |

Choose the most restrictive profile that supports your workload.

## Example Policies

### Read-Only Workspace Explorer

```toml
[sandbox]
name = "workspace-explorer"
max_timeout_ms = 10000
max_concurrent = 2

[commands]
allowed = ["ls", "cat", "head", "tail", "grep", "find", "wc"]
denied = ["sudo", "rm", "chmod"]

[filesystem]
read_paths = ["./workspace/**", "/usr/lib/**", "/lib/**"]
write_paths = []
execute_paths = ["/usr/bin/**", "/bin/**"]

[environment]
allowlist = ["PATH", "HOME", "LANG"]
forced = { SANDBOX = "1" }

[network]
mode = "none"

[seccomp]
profile = "shell_executor"
```

### Build Agent

```toml
[sandbox]
name = "build-agent"
max_timeout_ms = 300000
max_concurrent = 2

[commands]
allowed = ["cargo", "rustc", "rustfmt", "clippy-driver", "git", "ls", "cat", "grep", "find"]
denied = ["sudo", "rm", "chmod", "chown"]

[filesystem]
read_paths = ["./workspace/**", "/usr/lib/**", "/lib/**", "/etc/ssl/certs/**"]
write_paths = ["./workspace/target/**", "/tmp/cargo-build/**"]
execute_paths = ["/usr/bin/**", "/usr/local/bin/**", "/bin/**"]

[environment]
allowlist = ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUST_LOG", "LANG"]
forced = { SANDBOX = "1" }

[network]
mode = "allowlist"
allowed_hosts = ["crates.io:443", "static.crates.io:443", "github.com:443"]

[seccomp]
profile = "network_server"
```

## Best Practices

1. **Start restrictive, add permissions as needed.** Begin with `safe-default.toml`
   and only add what your workload requires.

2. **Use `denied` for safety nets.** Even with a narrow `allowed` list, explicitly
   deny dangerous commands as defense in depth.

3. **Minimize write paths.** Only grant write access to directories where output
   is expected.

4. **Prefer `none` network mode.** Most sandbox workloads don't need network access.

5. **Use `shell_executor` seccomp profile** unless you know you need something else.
   It's the sweet spot for most tool-runner workloads.

6. **Keep forced environment variables.** Always include `SANDBOX = "1"` so child
   processes can detect they're running in a sandbox.

7. **Test your policy.** Run the sandbox worker with `--strict` during testing to
   verify OS-level enforcement is working as expected.
