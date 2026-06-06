use anyhow::Result;
use std::path::Path;
use tokio::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)]
pub enum SandboxBackend {
    /// Software-level checks only (current behavior).
    #[default]
    PolicyOnly,
    /// macOS Seatbelt sandbox via sandbox-exec.
    #[cfg(target_os = "macos")]
    Seatbelt,
}

/// Wrap a command with sandbox restrictions.
#[allow(dead_code)]
pub fn wrap_command(command: &mut Command, cwd: &Path, backend: SandboxBackend) -> Result<()> {
    match backend {
        SandboxBackend::PolicyOnly => Ok(()),
        #[cfg(target_os = "macos")]
        SandboxBackend::Seatbelt => wrap_seatbelt(command, cwd),
    }
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn wrap_seatbelt(_command: &mut Command, cwd: &Path) -> Result<()> {
    // For now, log that Seatbelt would be applied but don't actually
    // wrap yet — this needs careful testing before enforcement.
    tracing::debug!(
        sandbox = "seatbelt",
        cwd = %cwd.display(),
        "seatbelt sandbox would apply here (not enforced yet)"
    );
    Ok(())
}

/// Generate a Seatbelt profile string for a given workspace.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn seatbelt_profile(workspace: &Path) -> String {
    let workspace = workspace.display();
    format!(
        r#"(version 1)
(deny default)
(allow file-read* (subpath "{workspace}"))
(allow file-write* (subpath "{workspace}"))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/bin"))
(allow file-read* (subpath "/Library"))
(allow file-read* (subpath "/System"))
(allow file-read* (subpath "/private/tmp"))
(allow file-write* (subpath "/private/tmp"))
(allow file-read* (literal "/dev/null"))
(allow file-read* (literal "/dev/urandom"))
(allow process-exec)
(allow process-fork)
(allow sysctl-read)
(deny network*)
"#
    )
}

/// Check if Seatbelt is available on this system.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn seatbelt_available() -> bool {
    std::path::Path::new("/usr/bin/sandbox-exec").exists()
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn seatbelt_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_only_is_noop() {
        let mut cmd = Command::new("echo");
        let result = wrap_command(&mut cmd, Path::new("/tmp"), SandboxBackend::PolicyOnly);
        assert!(result.is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_contains_workspace() {
        let profile = seatbelt_profile(Path::new("/Users/test/project"));
        assert!(profile.contains("/Users/test/project"));
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(deny default)"));
    }
}
