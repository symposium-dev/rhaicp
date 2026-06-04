use rhaicp::RhaiAgent;
use rhaicp::client::RhaiClient;

#[tokio::test]
async fn agent_returns_error_response() {
    // Agent script that throws — the error text gets sent back via say-like notification
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"let s = start_session(); s.prompt("throw \"agent error\";")"#,
        )
        .await
        .unwrap();
    // RhaiAgent sends error text as agent message chunk
    assert!(
        result.contains("agent error"),
        "expected error text, got: {result}"
    );
}

#[tokio::test]
async fn script_throws_after_prompt() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"
            let s = start_session();
            s.prompt("say(\"hi\")");
            throw "fail";
            "#,
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("fail"), "got: {err}");
}

#[tokio::test]
async fn script_throws_before_prompt() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"
            let s = start_session();
            throw "early";
            "#,
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("early"), "got: {err}");
}
