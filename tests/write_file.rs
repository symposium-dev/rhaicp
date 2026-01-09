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
    let _ = yopo::prompt(
        conductor(),
        r#"write_file("target/test.rs", "fn main() {}")"#,
    )
    .await?;

    expect_test::expect![[r#"
        "fn main() {}"
    "#]]
    .assert_debug_eq(&tokio::fs::read_to_string("target/test.rs").await.unwrap());

    Ok(())
}
