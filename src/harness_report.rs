use crate::config::{self, Config, PermissionMode, SandboxPolicy};
use crate::context;
use crate::telemetry;
use crate::thread_store;
use crate::tools;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct HarnessReport {
    pub ok: bool,
    pub checked_at: u64,
    pub cwd: PathBuf,
    pub summary: String,
    pub capabilities: Vec<HarnessCapability>,
    pub security_pipeline: Vec<SecurityStage>,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HarnessCapability {
    pub name: &'static str,
    pub status: HarnessStatus,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct SecurityStage {
    pub stage: &'static str,
    pub status: HarnessStatus,
    pub artifact: Option<PathBuf>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessStatus {
    Ready,
    Partial,
    Missing,
}

pub fn build_report(config: &Config) -> Result<HarnessReport> {
    let cwd = std::env::current_dir()
        .context("failed to read current directory")?
        .canonicalize()
        .context("failed to resolve current directory")?;
    let context = context::build_context(&cwd, None)?;
    let thread_count = thread_store::list_threads()?.len();
    let permission_mode = config::effective_permission_mode(config);
    let sandbox_policy = config::effective_sandbox_policy(config);

    let capabilities = vec![
        HarnessCapability {
            name: "rust_cli_core",
            status: HarnessStatus::Ready,
            detail: "single native Rust binary with Cargo as the source-of-truth toolchain"
                .to_string(),
        },
        HarnessCapability {
            name: "hosted_proxy_default",
            status: HarnessStatus::Ready,
            detail:
                "defaults to QC's hosted proxy, keeping upstream provider credentials server-side"
                    .to_string(),
        },
        HarnessCapability {
            name: "workspace_context",
            status: if context.stats.agent_doc_count > 0 {
                HarnessStatus::Ready
            } else {
                HarnessStatus::Partial
            },
            detail: format!(
                "{} repository instruction file(s), {} byte(s)",
                context.stats.agent_doc_count, context.stats.agent_doc_bytes
            ),
        },
        HarnessCapability {
            name: "tool_loop",
            status: HarnessStatus::Ready,
            detail: "native chat-completion tool calls plus tagged fallback tool blocks"
                .to_string(),
        },
        HarnessCapability {
            name: "command_policy",
            status: HarnessStatus::Ready,
            detail: format!(
                "{} approved argv prefix rule(s): {}",
                tools::allowed_prefix_labels(config).len(),
                tools::format_allowed_prefixes(config)
            ),
        },
        HarnessCapability {
            name: "permission_modes",
            status: if permission_mode == PermissionMode::ReadOnly {
                HarnessStatus::Ready
            } else {
                HarnessStatus::Partial
            },
            detail: format!(
                "current mode is {permission_mode}; read-only is available for inspection-only runs"
            ),
        },
        HarnessCapability {
            name: "approval_system",
            status: HarnessStatus::Ready,
            detail: format!(
                "3-tier approval mode (suggest/auto-edit/full-auto); current: {}",
                config::effective_approval_mode(config)
            ),
        },
        HarnessCapability {
            name: "deny_rules",
            status: HarnessStatus::Ready,
            detail: "dangerous command patterns (rm -rf, chmod 777, etc.) are always blocked"
                .to_string(),
        },
        HarnessCapability {
            name: "sandboxing",
            status: match sandbox_policy {
                SandboxPolicy::PolicyOnly => HarnessStatus::Partial,
            },
            detail: format!(
                "current policy is {sandbox_policy}; OS-level sandbox backends are not wired yet"
            ),
        },
        HarnessCapability {
            name: "thread_store",
            status: HarnessStatus::Ready,
            detail: format!("{thread_count} saved local thread(s) available for resume/state"),
        },
        HarnessCapability {
            name: "telemetry_trace",
            status: HarnessStatus::Ready,
            detail: format!(
                "local JSONL trace is {} at {}",
                if telemetry::enabled() {
                    "enabled"
                } else {
                    "disabled"
                },
                telemetry::trace_path()?.display()
            ),
        },
        HarnessCapability {
            name: "deterministic_mock_harness",
            status: if Path::new("tests/mock_proxy_harness.rs").exists() {
                HarnessStatus::Ready
            } else {
                HarnessStatus::Missing
            },
            detail: "integration test covers mock proxy diagnostics, status, and streaming"
                .to_string(),
        },
        HarnessCapability {
            name: "mcp_plugins_subagents",
            status: HarnessStatus::Missing,
            detail:
                "not implemented in this CLI yet; keep the first pass focused on safe local tools"
                    .to_string(),
        },
    ];

    let security_pipeline = security_stages(&cwd);
    let mut recommendations = Vec::new();

    if !cwd.join("THREAT_MODEL.md").exists() {
        recommendations.push(
            "Add a lightweight THREAT_MODEL.md before deeper autonomous security work.".to_string(),
        );
    }
    if sandbox_policy == SandboxPolicy::PolicyOnly {
        recommendations.push(
            "Add a real OS/container sandbox before executing target code as part of a security pipeline."
                .to_string(),
        );
    }
    if permission_mode != PermissionMode::ReadOnly {
        recommendations.push(
            "Use `--permission-mode read-only` for recon, threat modeling, and static triage runs."
                .to_string(),
        );
    }
    recommendations.push(
        "Next high-leverage gap: extend the mock harness to cover tool-call roundtrips, edit denial, and command-policy denial."
            .to_string(),
    );

    let ok = capabilities
        .iter()
        .all(|capability| capability.status != HarnessStatus::Missing);

    Ok(HarnessReport {
        ok,
        checked_at: now_secs(),
        cwd,
        summary: "Codeus has the safe local agent loop foundation; autonomous security execution still needs sandbox hardening and dedicated artifacts."
            .to_string(),
        capabilities,
        security_pipeline,
        recommendations,
    })
}

pub fn print_text(report: &HarnessReport) {
    println!("Codeus harness posture");
    println!("  cwd: {}", report.cwd.display());
    println!("  summary: {}", report.summary);
    println!();
    println!("Capabilities");
    for capability in &report.capabilities {
        println!(
            "  {:<28} {:<7} {}",
            capability.name,
            status_label(capability.status),
            capability.detail
        );
    }
    println!();
    println!("Security pipeline");
    for stage in &report.security_pipeline {
        let artifact = stage
            .artifact
            .as_ref()
            .map(|path| format!(" ({})", path.display()))
            .unwrap_or_default();
        println!(
            "  {:<12} {:<7} {}{}",
            stage.stage,
            status_label(stage.status),
            stage.detail,
            artifact
        );
    }
    println!();
    println!("Next moves");
    for item in &report.recommendations {
        println!("  - {item}");
    }
}

fn security_stages(cwd: &Path) -> Vec<SecurityStage> {
    vec![
        stage(
            cwd,
            "recon",
            "THREAT_MODEL.md",
            "threat model artifact for scoped review",
        ),
        stage(
            cwd,
            "find",
            "VULN-FINDINGS.json",
            "candidate finding list from static or agent-assisted scanning",
        ),
        stage(
            cwd,
            "triage",
            "TRIAGE.json",
            "deduped and ranked finding set",
        ),
        stage(
            cwd,
            "report",
            "VULN-FINDINGS.md",
            "human-readable finding report",
        ),
        stage_dir(
            cwd,
            "patch",
            "PATCHES",
            "candidate fixes with verification notes",
        ),
    ]
}

fn stage(cwd: &Path, name: &'static str, file: &str, detail: &'static str) -> SecurityStage {
    let path = cwd.join(file);
    SecurityStage {
        stage: name,
        status: if path.exists() {
            HarnessStatus::Ready
        } else {
            HarnessStatus::Missing
        },
        artifact: Some(path),
        detail: detail.to_string(),
    }
}

fn stage_dir(cwd: &Path, name: &'static str, dir: &str, detail: &'static str) -> SecurityStage {
    let path = cwd.join(dir);
    SecurityStage {
        stage: name,
        status: if path.is_dir() {
            HarnessStatus::Ready
        } else {
            HarnessStatus::Missing
        },
        artifact: Some(path),
        detail: detail.to_string(),
    }
}

fn status_label(status: HarnessStatus) -> &'static str {
    match status {
        HarnessStatus::Ready => "ready",
        HarnessStatus::Partial => "partial",
        HarnessStatus::Missing => "missing",
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
