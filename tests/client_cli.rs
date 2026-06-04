use std::process::Command;

fn rhaicp_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rhaicp"))
}

#[test]
fn cli_runs_script_against_self() {
    let script_path = "target/test_cli_script.rhai";
    std::fs::write(
        script_path,
        r#"let s = start_session(); s.prompt("say(\"it works\")")"#,
    )
    .unwrap();

    let output = rhaicp_bin()
        .args(["client", "--script", script_path, "--"])
        .arg(env!("CARGO_BIN_EXE_rhaicp"))
        .arg("acp")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("it works"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn cli_bad_agent_command() {
    let script_path = "target/test_cli_bad_agent.rhai";
    std::fs::write(
        script_path,
        r#"let s = start_session(); s.prompt("say(\"hi\")")"#,
    )
    .unwrap();

    let output = rhaicp_bin()
        .args([
            "client",
            "--script",
            script_path,
            "--",
            "nonexistent-command-xyz",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
}

#[test]
fn cli_missing_script_file() {
    let output = rhaicp_bin()
        .args([
            "client",
            "--script",
            "nonexistent.rhai",
            "--",
            env!("CARGO_BIN_EXE_rhaicp"),
            "acp",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Failed to read script"), "stderr: {stderr}");
}

#[test]
fn cli_exit_code_on_script_error() {
    let script_path = "target/test_cli_throw.rhai";
    std::fs::write(script_path, r#"throw "script error";"#).unwrap();

    let output = rhaicp_bin()
        .args([
            "client",
            "--script",
            script_path,
            "--",
            env!("CARGO_BIN_EXE_rhaicp"),
            "acp",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("script error"), "stderr: {stderr}");
}
