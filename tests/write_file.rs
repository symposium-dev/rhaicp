//! Integration test for rhaicp write_file() functionality.

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
async fn test_write_file() -> Result<(), agent_client_protocol::Error> {
    let _ = support::prompt(
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
