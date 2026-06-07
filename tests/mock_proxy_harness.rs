use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn consumer_cli_accepts_only_bare_codeus() {
    let root = temp_root("codeus-consumer-cli");
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();

    let bare = codeus(&home, &workspace).output().unwrap();
    assert!(
        bare.status.success(),
        "bare codeus failed: {}",
        String::from_utf8_lossy(&bare.stderr)
    );

    for args in [
        &["run", "say hello"][..],
        &["chat"][..],
        &["login"][..],
        &["account"][..],
        &["usage"][..],
        &["doctor"][..],
        &["harness"][..],
        &["status", "--output-format", "json"][..],
        &["state"][..],
        &["update"][..],
        &["config", "show"][..],
        &["config", "set", "api_base", "https://api.example.test/v1"][..],
        &["config", "set", "model", "sonnet-4.6"][..],
    ] {
        let output = codeus(&home, &workspace).args(args).output().unwrap();
        assert!(
            !output.status.success(),
            "expected codeus {args:?} to be rejected"
        );
    }

    let _ = fs::remove_dir_all(root);
}

fn codeus(home: &PathBuf, workspace: &PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codeus"));
    command
        .current_dir(workspace)
        .env("HOME", home)
        .env("CODEUS_NO_UPDATE_CHECK", "1")
        .env_remove("CODEUS_SESSION_TOKEN")
        .env_remove("CODEUS_API_BASE")
        .env_remove("CODEUS_MODEL")
        .env_remove("CODEUS_PERMISSION_MODE")
        .env_remove("CODEUS_OUTPUT_FORMAT");
    command
}

fn temp_root(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{name}-{nonce}"))
}
