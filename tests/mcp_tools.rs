//! Integration tests for MCP tool access via Rhai scripts.
//!
//! These tests use the conductor + proxy pattern to provide in-process MCP servers
//! that the Rhai agent can call via `mcp::list_tools` and `mcp::call_tool`.

mod support;

use agent_client_protocol::mcp_server::McpServer;
use agent_client_protocol::{Client, Conductor, ConnectTo, DynConnectTo, Proxy};
use agent_client_protocol_conductor::{ConductorImpl, ProxiesAndAgent};
use agent_client_protocol_polyfill::mcp_over_acp::McpOverAcpPolyfill;
use agent_client_protocol_rmcp::McpServerExt;
use rhaicp::RhaiAgent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

/// Create a proxy that provides an echo MCP server
fn create_echo_proxy() -> DynConnectTo<Conductor> {
    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct EchoInput {
        message: String,
    }

    let mcp_server = McpServer::<Conductor>::builder("echo")
        .instructions("Echo server for testing")
        .tool_fn(
            "echo",
            "Echoes back the input message",
            async |input: EchoInput, _cx| Ok(format!("Echo: {}", input.message)),
            agent_client_protocol_rmcp::tool_fn!(),
        )
        .build();

    DynConnectTo::new(
        Proxy
            .builder()
            .name("echo-proxy")
            .with_mcp_server(mcp_server),
    )
}

fn conductor_with_echo() -> impl ConnectTo<Client> {
    ConductorImpl::new_agent(
        "test-conductor",
        ProxiesAndAgent::new(TestRhaiAgent)
            .proxy(create_echo_proxy())
            .proxy(McpOverAcpPolyfill::http()),
    )
}

#[tokio::test]
async fn test_list_tools() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_echo(),
        r#"
        let tools = mcp::list_tools("echo");
        for tool in tools {
            say(tool + "\n");
        }
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "echo\n"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_call_tool() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_echo(),
        r#"
        let result = mcp::call_tool("echo", "echo", #{ message: "Hello from Rhai!" });
        say(result);
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Echo: Hello from Rhai!"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

/// Create a proxy with a calculator MCP server for more complex tool testing
fn create_calculator_proxy() -> DynConnectTo<Conductor> {
    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct AddInput {
        a: i64,
        b: i64,
    }

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct MultiplyInput {
        a: i64,
        b: i64,
    }

    let mcp_server = McpServer::<Conductor>::builder("calc")
        .instructions("Calculator server for testing")
        .tool_fn(
            "add",
            "Add two numbers",
            async |input: AddInput, _cx| Ok(input.a + input.b),
            agent_client_protocol_rmcp::tool_fn!(),
        )
        .tool_fn(
            "multiply",
            "Multiply two numbers",
            async |input: MultiplyInput, _cx| Ok(input.a * input.b),
            agent_client_protocol_rmcp::tool_fn!(),
        )
        .build();

    DynConnectTo::new(
        Proxy
            .builder()
            .name("calc-proxy")
            .with_mcp_server(mcp_server),
    )
}

fn conductor_with_calc() -> impl ConnectTo<Client> {
    ConductorImpl::new_agent(
        "test-conductor",
        ProxiesAndAgent::new(TestRhaiAgent)
            .proxy(create_calculator_proxy())
            .proxy(McpOverAcpPolyfill::http()),
    )
}

#[tokio::test]
async fn test_list_multiple_tools() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_calc(),
        r#"
        let tools = mcp::list_tools("calc");
        say("Tools: " + tools.len().to_string());
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Tools: 2"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_call_add_tool() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_calc(),
        r#"
        let result = mcp::call_tool("calc", "add", #{ a: 3, b: 5 });
        say(result.to_string());
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "8"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_call_multiply_tool() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_calc(),
        r#"
        let result = mcp::call_tool("calc", "multiply", #{ a: 7, b: 6 });
        say(result.to_string());
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "42"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_chain_tool_calls() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_calc(),
        r#"
        // Calculate (3 + 5) * 2 = 16
        let sum = mcp::call_tool("calc", "add", #{ a: 3, b: 5 });

        let product = mcp::call_tool("calc", "multiply", #{ a: sum, b: 2 });
        say("Result: " + product.to_string());
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Result: 16"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

#[tokio::test]
async fn test_unknown_server_error() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_echo(),
        r#"
        let tools = mcp::list_tools("nonexistent");
        say(tools);
        "#,
    )
    .await?;

    // Should contain an error message about the server not being found
    assert!(
        result.contains("ERROR"),
        "Expected error message, got: {}",
        result
    );

    Ok(())
}

// =============================================================================
// Structured vs Unstructured Content Tests
// =============================================================================

/// Create a proxy with a tool that returns a struct (structured content)
fn create_structured_proxy() -> DynConnectTo<Conductor> {
    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct GetUserInput {
        id: i64,
    }

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct UserInfo {
        name: String,
        age: i64,
    }

    let mcp_server = McpServer::<Conductor>::builder("users")
        .instructions("User info server for testing structured content")
        .tool_fn(
            "get_user",
            "Get user info by ID",
            async |input: GetUserInput, _cx| {
                Ok(UserInfo {
                    name: format!("User{}", input.id),
                    age: 20 + input.id,
                })
            },
            agent_client_protocol_rmcp::tool_fn!(),
        )
        .build();

    DynConnectTo::new(
        Proxy
            .builder()
            .name("structured-proxy")
            .with_mcp_server(mcp_server),
    )
}

fn conductor_with_structured() -> impl ConnectTo<Client> {
    ConductorImpl::new_agent(
        "test-conductor",
        ProxiesAndAgent::new(TestRhaiAgent)
            .proxy(create_structured_proxy())
            .proxy(McpOverAcpPolyfill::http()),
    )
}

/// Test structured content: tool returns a struct, result has `structured_content`
#[tokio::test]
async fn test_structured_content_returns_object() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_structured(),
        r#"
        let user = mcp::call_tool("users", "get_user", #{ id: 42 });
        // Access struct fields directly - structured content is returned as a Rhai map
        say("Name: " + user.name + ", Age: " + user.age.to_string());
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Name: User42, Age: 62"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

/// Test unstructured content: tool returns a primitive, text content is parsed as JSON
#[tokio::test]
async fn test_unstructured_content_preserves_number_types(
) -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_calc(),
        r#"
        let sum = mcp::call_tool("calc", "add", #{ a: 100, b: 200 });
        // If we preserved the number type, we can do arithmetic
        let doubled = sum * 2;
        say("Sum doubled: " + doubled.to_string());
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Sum doubled: 600"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}

/// Test unstructured content: tool returns a string
#[tokio::test]
async fn test_unstructured_content_returns_string() -> Result<(), agent_client_protocol::Error> {
    let result = support::prompt(
        conductor_with_echo(),
        r#"
        let msg = mcp::call_tool("echo", "echo", #{ message: "test" });
        // String results can be used directly
        say("Got: " + msg);
        "#,
    )
    .await?;

    expect_test::expect![[r#"
        "Got: Echo: test"
    "#]]
    .assert_debug_eq(&result);

    Ok(())
}
