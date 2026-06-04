//! Integration test for rhaicp basic say() functionality.

mod support;

use agent_client_protocol::{Client, ConnectTo};
use agent_client_protocol_conductor::{ConductorImpl, ProxiesAndAgent};
use rhaicp::RhaiAgent;

/// Wrapper to make RhaiAgent work with the test infrastructure
struct TestRhaiAgent;

impl ConnectTo<Client> for TestRhaiAgent {
    async fn connect_to(
        self,
        client: impl ConnectTo<agent_client_protocol::Agent>,
    ) -> Result<(), agent_client_protocol::Error> {
        RhaiAgent::new().connect_to(client).await
    }
}

fn conductor() -> impl ConnectTo<Client> {
    ConductorImpl::new_agent("test-conductor", ProxiesAndAgent::new(TestRhaiAgent))
}

#[tokio::test]
async fn test_say_hello_world() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(conductor(), r#"say("Hello, "); say("World!")"#).await?;

    expect_test::expect![[r#"
        "Hello, World!"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_say_multiline() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor(),
        r#"
        say("Line 1\n");
        say("Line 2\n");
        say("Line 3");
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Line 1\nLine 2\nLine 3"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_user_request_extraction() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor(),
        r#"Some preamble text <userRequest>say("Extracted!")</userRequest> some trailing text"#,
    )
    .await?;

    expect_test::expect![[r#"
        "Extracted!"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_rhai_error_handling() -> Result<(), agent_client_protocol::Error> {
    let result =
        support::prompt(conductor(), r#"this is not valid rhai syntax {"#).await?;

    expect_test::expect![[r#"
        "Rhai error: Syntax error: 'this' can only be used in functions (line 1, position 1)"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}
