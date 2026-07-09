//! Positive/negative destructive-pattern corpus for the L0 hardline floor.
//!
//! The positive corpus is the classic destructive-command set from the
//! Execution Safety Floor proposal's Slice 0 verification section. The
//! negative corpus is a hard gate: false positives on this floor can't be
//! overridden by any approval, so every command here must be allowed
//! through unconditionally by L0 (higher layers, not implemented in this
//! slice, may still gate them).

use exec_guard::detect_hardline;

const MUST_TRIP: &[&str] = &[
    "rm -rf /",
    r#"rm -rf "/""#,
    "rm -rf $HOME",
    "rm -rf ${HOME}",
    "sudo rm -rf /*",
    "mkfs.ext4 /dev/sda",
    "dd if=/dev/zero of=/dev/sda",
    ":(){ :|:& };:",
    "shutdown -h now",
    "reboot",
    "echo x | $(rm -rf /)",
];

const MUST_NOT_TRIP: &[&str] = &[
    r#"git commit -m "block rm -rf / spellings""#,
    "grep 'shutdown' aiua.log",
    "rm -rf /tmp/build",
    "echo reboot",
];

#[test]
fn positive_corpus_is_blocked() {
    for cmd in MUST_TRIP {
        assert!(
            detect_hardline(cmd).is_some(),
            "expected hardline match for destructive command: {cmd:?}"
        );
    }
}

#[test]
fn negative_corpus_passes_through() {
    for cmd in MUST_NOT_TRIP {
        assert!(
            detect_hardline(cmd).is_none(),
            "expected no hardline match for benign command: {cmd:?}"
        );
    }
}

// ── Additional coverage for rules beyond the proposal's minimal corpus ────
// (single-quoted root, extra rm flag orderings, and every hardline rule
// this slice ships, so a regression in any one rule fails loudly here
// rather than only showing up once a later slice adds L1-L4 coverage.)

#[test]
fn quoted_root_paths_both_quote_styles() {
    assert!(detect_hardline(r#"rm -rf "/""#).is_some());
    assert!(detect_hardline("rm -rf '/'").is_some());
}

#[test]
fn rm_flag_order_variants() {
    assert!(detect_hardline("rm -fr /").is_some());
    assert!(detect_hardline("rm --recursive --force /").is_some());
    assert!(detect_hardline("rm -rf /").is_some());
}

#[test]
fn protected_system_directories() {
    for dir in [
        "/home", "/root", "/etc", "/usr", "/var", "/bin", "/sbin", "/boot", "/lib",
    ] {
        let cmd = format!("rm -rf {dir}");
        assert!(
            detect_hardline(&cmd).is_some(),
            "expected hardline match for protected dir: {cmd:?}"
        );
    }
}

#[test]
fn protected_system_directories_trailing_slash() {
    // Shell tab-completion appends a trailing slash (`rm -rf /etc<TAB>` ->
    // `rm -rf /etc/`) — the floor must still trip on that form, not just the
    // bare name or the explicit `/*` glob.
    for dir in [
        "/home", "/root", "/etc", "/usr", "/var", "/bin", "/sbin", "/boot", "/lib",
    ] {
        let cmd = format!("rm -rf {dir}/");
        assert!(
            detect_hardline(&cmd).is_some(),
            "expected hardline match for protected dir with trailing slash: {cmd:?}"
        );
    }
}

#[test]
fn tilde_home_delete() {
    assert!(detect_hardline("rm -rf ~").is_some());
    assert!(detect_hardline("rm -rf ~/").is_some());
}

#[test]
fn raw_block_device_redirect() {
    assert!(detect_hardline("echo bad > /dev/sda").is_some());
    assert!(detect_hardline("cat payload > /dev/nvme0n1").is_some());
}

#[test]
fn kill_all_processes() {
    assert!(detect_hardline("kill -1").is_some());
}

#[test]
fn shutdown_family_command_position() {
    assert!(detect_hardline("halt").is_some());
    assert!(detect_hardline("poweroff").is_some());
    assert!(detect_hardline("init 0").is_some());
    assert!(detect_hardline("init 6").is_some());
    assert!(detect_hardline("systemctl poweroff").is_some());
    assert!(detect_hardline("systemctl kexec").is_some());
    assert!(detect_hardline("telinit 0").is_some());
}

#[test]
fn chained_command_position_still_anchors() {
    // A benign first command followed by a destructive one after a
    // separator must still trip — the anchor is about command *position*,
    // not "must be the first word in the whole string".
    assert!(detect_hardline("echo hi; rm -rf /").is_some());
    assert!(detect_hardline("echo hi && shutdown -h now").is_some());
    assert!(detect_hardline("echo hi | shutdown -h now").is_some());
}

#[test]
fn sudo_env_exec_wrappers_do_not_hide_the_command() {
    assert!(detect_hardline("sudo shutdown -h now").is_some());
    assert!(detect_hardline("env FOO=bar reboot").is_some());
    assert!(detect_hardline("exec rm -rf /").is_some());
}

#[test]
fn sudo_separate_token_value_flags_still_realign_on_the_command() {
    // `-u root`, `--user root`, etc. take their value as a *separate* shell
    // word (not `-uroot`/`--user=root`). Ordinary, non-obfuscated sudo
    // invocations inside the stated threat model must still trip the floor.
    assert!(detect_hardline("sudo -u root rm -rf /").is_some());
    assert!(detect_hardline("sudo --user root reboot").is_some());
    assert!(detect_hardline("sudo -g wheel rm -rf /").is_some());
    assert!(detect_hardline("sudo --group wheel rm -rf /").is_some());
    assert!(detect_hardline("sudo -U root rm -rf /").is_some());
    assert!(detect_hardline("sudo -p prompt: rm -rf /").is_some());
    assert!(detect_hardline("sudo -C 3 rm -rf /").is_some());
    assert!(detect_hardline("sudo -h remotehost rm -rf /").is_some());
    assert!(detect_hardline("sudo -D /tmp rm -rf /").is_some());
    assert!(detect_hardline("sudo --chdir /tmp rm -rf /").is_some());
    // Mixed with a value-less flag before/after the value-taking one.
    assert!(detect_hardline("sudo -E -u root rm -rf /").is_some());
    assert!(detect_hardline("sudo -u root -E rm -rf /").is_some());
}

#[test]
fn philotic_self_termination_rules() {
    assert!(detect_hardline("launchctl bootout system/com.philotic.aiua.mac-jane").is_some());
    assert!(
        detect_hardline("launchctl unload ~/Library/LaunchAgents/com.philotic.aiua.plist")
            .is_some()
    );
    assert!(detect_hardline("pkill aiua").is_some());
    assert!(detect_hardline("killall aiua").is_some());
    // A different binary named similarly should not trip the aiua-specific rule.
    assert!(detect_hardline("pkill aiuathing").is_none());
}

#[test]
fn ifs_and_empty_quote_obfuscation_still_caught() {
    assert!(detect_hardline("rm${IFS}-rf${IFS}/").is_some());
    assert!(detect_hardline("r''m -rf /").is_some());
}

#[test]
fn denial_message_says_do_not_retry() {
    let m = detect_hardline("rm -rf /").expect("must match");
    assert!(m.denial_message().to_lowercase().contains("do not retry"));
}
