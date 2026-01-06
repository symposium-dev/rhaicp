//! Integration test for rhaicp basic say() functionality.
//!
//! This test verifies that:
//! 1. RhaiAgent can be connected via a Conductor
//! 2. A simple Rhai script with say() calls works
//! 3. Multiple say() calls stream output correctly

use rhaicp::RhaiAgent;
use sacp::Component;
use sacp::link::AgentToClient;
use sacp_conductor::{Conductor, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

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

#[tokio::test]
async fn test_say_hello_world() -> Result<(), sacp::Error> {
    // Initialize tracing for debugging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    // Create duplex streams for editor <-> conductor communication
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    // Spawn the conductor with our RhaiAgent
    let conductor_handle = tokio::spawn(async move {
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(TestRhaiAgent),
            Default::default(),
        )
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    // Send the Rhai script and get the result
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
        let result = yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            r#"say("Hello, "); say("World!")"#,
        )
        .await?;

        tracing::debug!(?result, "Received response from RhaiAgent");

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    // The result should be the concatenated output of both say() calls
    expect_test::expect![[r#"
        "Hello, World!"
    "#]]
    .assert_debug_eq(&result);

    tracing::info!(?result, "Test completed successfully");

    conductor_handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_say_multiline() -> Result<(), sacp::Error> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let conductor_handle = tokio::spawn(async move {
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(TestRhaiAgent),
            Default::default(),
        )
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
        let result = yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            r#"
            say("Line 1\n");
            say("Line 2\n");
            say("Line 3");
            "#,
        )
        .await?;

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    expect_test::expect![[r#"
        "Line 1\nLine 2\nLine 3"
    "#]]
    .assert_debug_eq(&result);

    conductor_handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_user_request_extraction() -> Result<(), sacp::Error> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let conductor_handle = tokio::spawn(async move {
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(TestRhaiAgent),
            Default::default(),
        )
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
        // The script is embedded in <userRequest> tags with surrounding text
        let result = yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            r#"Some preamble text <userRequest>say("Extracted!")</userRequest> some trailing text"#,
        )
        .await?;

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    expect_test::expect![[r#"
        "Extracted!"
    "#]]
    .assert_debug_eq(&result);

    conductor_handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_rhai_error_handling() -> Result<(), sacp::Error> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let conductor_handle = tokio::spawn(async move {
        Conductor::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(TestRhaiAgent),
            Default::default(),
        )
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
        // Send invalid Rhai syntax
        let result = yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            r#"this is not valid rhai syntax {"#,
        )
        .await?;

        Ok::<String, sacp::Error>(result)
    })
    .await
    .expect("Test timed out")
    .expect("Editor failed");

    // Should contain an error message about syntax
    expect_test::expect![[r#"
        "Rhai error: Syntax error: 'this' can only be used in functions (line 1, position 1)"
    "#]]
    .assert_debug_eq(&result);

    conductor_handle.abort();

    Ok(())
}
