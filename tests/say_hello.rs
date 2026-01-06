//! Integration test for rhaicp basic say() functionality.

use rhaicp::RhaiAgent;
use sacp::Component;
use sacp::link::AgentToClient;
use sacp_conductor::{Conductor, ProxiesAndAgent};

/// Wrapper to make RhaiAgent work with the test infrastructure
struct TestRhaiAgent;

impl Component<AgentToClient> for TestRhaiAgent {
    async fn serve(
        self,
        client: impl Component<sacp::link::ClientToAgent>,
    ) -> Result<(), sacp::Error> {
        Component::<AgentToClient>::serve(RhaiAgent::new(), client).await
    }
}

fn conductor() -> impl sacp::Component<sacp::link::AgentToClient> {
    Conductor::new_agent(
        "test-conductor".to_string(),
        ProxiesAndAgent::new(TestRhaiAgent),
        Default::default(),
    )
}

#[tokio::test]
async fn test_say_hello_world() -> Result<(), sacp::Error> {
    let result = yopo::prompt(conductor(), r#"say("Hello, "); say("World!")"#).await?;

    expect_test::expect![[r#"
        "Hello, World!"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_say_multiline() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_user_request_extraction() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_rhai_error_handling() -> Result<(), sacp::Error> {
    let result = yopo::prompt(conductor(), r#"this is not valid rhai syntax {"#).await?;

    expect_test::expect![[r#"
        "Rhai error: Syntax error: 'this' can only be used in functions (line 1, position 1)"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}
