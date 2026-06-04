use rhaicp::RhaiAgent;
use rhaicp::client::RhaiClient;

#[tokio::test]
async fn single_prompt_returns_agent_say_output() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"let s = start_session(); s.prompt("say(\"hello world\")")"#,
        )
        .await
        .unwrap();
    assert_eq!(result, "hello world");
}

#[tokio::test]
async fn multi_turn_conversation() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"
            let s = start_session();
            let r1 = s.prompt("say(\"first\")");
            let r2 = s.prompt("say(\"second\")");
            r1 + " " + r2
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "first second");
}

#[tokio::test]
async fn script_that_never_calls_start_session() {
    let result = RhaiClient::new()
        .execute(RhaiAgent::new(), "40 + 2")
        .await
        .unwrap();
    assert_eq!(result, "42");
}

#[tokio::test]
async fn multiple_sessions() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"
            let s1 = start_session();
            let s2 = start_session();
            let r1 = s1.prompt("say(\"from s1\")");
            let r2 = s2.prompt("say(\"from s2\")");
            r1 + " | " + r2
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "from s1 | from s2");
}

#[tokio::test]
async fn empty_prompt() {
    let result = RhaiClient::new()
        .execute(RhaiAgent::new(), r#"let s = start_session(); s.prompt("")"#)
        .await
        .unwrap();
    // Empty script produces no say() output
    assert_eq!(result, "");
}

#[tokio::test]
async fn agent_say_multiple_chunks() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"let s = start_session(); s.prompt("say(\"hello \"); say(\"world\")")"#,
        )
        .await
        .unwrap();
    assert_eq!(result, "hello world");
}

#[tokio::test]
async fn cwd_is_passed_to_session() {
    let result = RhaiClient::new()
        .cwd("/tmp")
        .execute(
            RhaiAgent::new(),
            r#"let s = start_session(); s.prompt("say(cwd())")"#,
        )
        .await
        .unwrap();
    assert_eq!(result, "/tmp");
}
