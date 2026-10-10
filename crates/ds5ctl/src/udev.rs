//! One-time installation of the udev rule that lets ds5ctl run without root.

use std::io::Write;
use std::process::{Command, Stdio};

pub const RULE: &str = include_str!("../../../dist/70-ds5ctl.rules");
pub const RULE_PATH: &str = "/etc/udev/rules.d/70-ds5ctl.rules";

/// Writes the rule and re-triggers hidraw devices, asking for the admin
/// password through polkit unless we already run as root.
pub fn install() -> Result<(), String> {
    let script = format!(
        "cat > {RULE_PATH} && udevadm control --reload && udevadm trigger --subsystem-match=hidraw"
    );
    // SAFETY: geteuid has no preconditions.
    let mut command = if unsafe { libc::geteuid() } == 0 {
        Command::new("/bin/sh")
    } else {
        let mut pkexec = Command::new("pkexec");
        pkexec.arg("/bin/sh");
        pkexec
    };
    let mut child = command
        .args(["-c", &script])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run pkexec: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(RULE.as_bytes())
            .map_err(|e| format!("cannot write the rule: {e}"))?;
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("installing the udev rule failed ({status})"))
    }
}
