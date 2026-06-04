use agent_client_protocol::schema::SessionId;
use rhaicp::client::RhaiClient;
use rhaicp::{PriorSession, RhaiAgent};

#[tokio::test]
async fn list_sessions_returns_prior_session_ids() {
    let agent = RhaiAgent::new().prior_sessions(vec![
        PriorSession {
            session_id: SessionId::new("sess_one"),
            script: r#"let p = receive_prompt(); say(p);"#.to_string(),
        },
        PriorSession {
            session_id: SessionId::new("sess_two"),
            script: r#"let p = receive_prompt(); say(p);"#.to_string(),
        },
    ]);

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let sessions = list_sessions();
            sessions.len()
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "2");
}

#[tokio::test]
async fn list_sessions_empty_when_no_prior_sessions() {
    let result = RhaiClient::new()
        .execute(
            RhaiAgent::new(),
            r#"
            let sessions = list_sessions();
            sessions.len()
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "0");
}

#[tokio::test]
async fn load_session_replays_history() {
    let agent = RhaiAgent::new().prior_sessions(vec![PriorSession {
        session_id: SessionId::new("sess_abc"),
        script: r#"
            if is_load() {
                user("Fix the bug");
                say("I found the issue");
            }
            let p = receive_prompt();
            say("got: " + p);
        "#
        .to_string(),
    }]);

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let s = load_session("sess_abc");
            let u = s.updates();
            // Should have replay updates: user + agent message
            u.len()
            "#,
        )
        .await
        .unwrap();
    // The no-op prompt we send internally also generates a "got: " response,
    // so we expect: user_message_chunk + agent_message_chunk (replay) + agent_message_chunk (from empty prompt)
    assert_eq!(result, "3");
}

#[tokio::test]
async fn load_session_replay_content() {
    let agent = RhaiAgent::new().prior_sessions(vec![PriorSession {
        session_id: SessionId::new("sess_abc"),
        script: r#"
            if is_load() {
                user("Fix the bug");
                say("I found the issue");
            }
            let p = receive_prompt();
            say("prompt was: " + p);
        "#
        .to_string(),
    }]);

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let s = load_session("sess_abc");
            let u = s.updates();
            u[0].type + ":" + u[0].text + "|" + u[1].type + ":" + u[1].text
            "#,
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        "user_message_chunk:Fix the bug|agent_message_chunk:I found the issue"
    );
}

#[tokio::test]
async fn resume_session_skips_replay() {
    let agent = RhaiAgent::new().prior_sessions(vec![PriorSession {
        session_id: SessionId::new("sess_abc"),
        script: r#"
            if is_load() {
                user("Fix the bug");
                say("I found the issue");
            }
            let p = receive_prompt();
            say("resumed: " + p);
        "#
        .to_string(),
    }]);

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let s = resume_session("sess_abc");
            let response = s.prompt("continue please");
            response
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "resumed: continue please");
}

#[tokio::test]
async fn scripted_session_multi_turn() {
    let agent = RhaiAgent::new().prior_sessions(vec![PriorSession {
        session_id: SessionId::new("sess_multi"),
        script: r#"
            let p1 = receive_prompt();
            say("first: " + p1);
            let p2 = receive_prompt();
            say("second: " + p2);
        "#
        .to_string(),
    }]);

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let s = resume_session("sess_multi");
            let r1 = s.prompt("hello");
            let r2 = s.prompt("world");
            r1 + " | " + r2
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "first: hello | second: world");
}

#[tokio::test]
async fn new_session_with_script() {
    let agent = RhaiAgent::new().new_session_script(
        r#"
        let p = receive_prompt();
        say("scripted: " + p);
        "#
        .to_string(),
    );

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let s = start_session();
            s.prompt("test input")
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "scripted: test input");
}

#[tokio::test]
async fn session_id_accessible() {
    let agent = RhaiAgent::new().prior_sessions(vec![PriorSession {
        session_id: SessionId::new("my_session"),
        script: r#"let p = receive_prompt(); say(p);"#.to_string(),
    }]);

    let result = RhaiClient::new()
        .execute(
            agent,
            r#"
            let s = resume_session("my_session");
            s.session_id()
            "#,
        )
        .await
        .unwrap();
    assert_eq!(result, "my_session");
}
