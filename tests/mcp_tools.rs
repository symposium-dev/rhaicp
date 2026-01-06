//! Integration tests for MCP tool access via Rhai scripts.
//!
//! These tests use the conductor + proxy pattern to provide in-process MCP servers
//! that the Rhai agent can call via `mcp::list_tools` and `mcp::call_tool`.

use rhaicp::RhaiAgent;
use sacp::mcp_server::McpServer;
use sacp::link::AgentToClient;
use sacp::{Component, ProxyToConductor};
use sacp_conductor::{Conductor, ProxiesAndAgent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

/// Create a proxy that provides an echo MCP server
fn create_echo_proxy() -> Result<sacp::DynComponent<ProxyToConductor>, sacp::Error> {
    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct EchoInput {
        message: String,
    }

    let mcp_server = McpServer::builder("echo")
        .instructions("Echo server for testing")
        .tool_fn(
            "echo",
            "Echoes back the input message",
            async |input: EchoInput, _context| Ok(format!("Echo: {}", input.message)),
            sacp::tool_fn!(),
        )
        .build();

    Ok(sacp::DynComponent::new(EchoProxy { mcp_server }))
}

struct EchoProxy<R: sacp::JrResponder<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::JrResponder<ProxyToConductor> + 'static + Send> Component<ProxyToConductor>
    for EchoProxy<R>
{
    async fn serve(
        self,
        client: impl Component<sacp::link::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("echo-proxy")
            .with_mcp_server(self.mcp_server)
            .serve(client)
            .await
    }
}

fn conductor_with_echo() -> impl Component<AgentToClient> {
    Conductor::new_agent(
        "test-conductor".to_string(),
        ProxiesAndAgent::new(TestRhaiAgent).proxy(create_echo_proxy().unwrap()),
        Default::default(),
    )
}

#[tokio::test]
async fn test_list_tools() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_call_tool() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
fn create_calculator_proxy() -> Result<sacp::DynComponent<ProxyToConductor>, sacp::Error> {
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

    let mcp_server = McpServer::builder("calc")
        .instructions("Calculator server for testing")
        .tool_fn(
            "add",
            "Add two numbers",
            async |input: AddInput, _context| Ok(input.a + input.b),
            sacp::tool_fn!(),
        )
        .tool_fn(
            "multiply",
            "Multiply two numbers",
            async |input: MultiplyInput, _context| Ok(input.a * input.b),
            sacp::tool_fn!(),
        )
        .build();

    Ok(sacp::DynComponent::new(CalculatorProxy { mcp_server }))
}

struct CalculatorProxy<R: sacp::JrResponder<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::JrResponder<ProxyToConductor> + 'static + Send> Component<ProxyToConductor>
    for CalculatorProxy<R>
{
    async fn serve(
        self,
        client: impl Component<sacp::link::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("calc-proxy")
            .with_mcp_server(self.mcp_server)
            .serve(client)
            .await
    }
}

fn conductor_with_calc() -> impl Component<AgentToClient> {
    Conductor::new_agent(
        "test-conductor".to_string(),
        ProxiesAndAgent::new(TestRhaiAgent).proxy(create_calculator_proxy().unwrap()),
        Default::default(),
    )
}

#[tokio::test]
async fn test_list_multiple_tools() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_call_add_tool() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_call_multiply_tool() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_chain_tool_calls() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
async fn test_unknown_server_error() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
        conductor_with_echo(),
        r#"
        let tools = mcp::list_tools("nonexistent");
        say(tools);
        "#,
    )
    .await?;

    // Should contain an error message about the server not being found
    assert!(result.contains("ERROR"), "Expected error message, got: {}", result);

    Ok(())
}

// =============================================================================
// Structured vs Unstructured Content Tests
// =============================================================================
//
// MCP tools can return results in two ways:
// 1. Structured content: When a tool returns a struct type, the result includes
//    `structured_content` with the JSON value directly.
// 2. Unstructured content: When a tool returns a primitive type (String, i64, etc.),
//    the result is serialized to text in the `content` array.
//
// The tests above (echo, add, multiply) all use unstructured content because they
// return primitive types. The tests below explicitly test both paths.

/// Create a proxy with a tool that returns a struct (structured content)
fn create_structured_proxy() -> Result<sacp::DynComponent<ProxyToConductor>, sacp::Error> {
    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct GetUserInput {
        id: i64,
    }

    /// User info - returning a struct triggers structured content
    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct UserInfo {
        name: String,
        age: i64,
    }

    let mcp_server = McpServer::builder("users")
        .instructions("User info server for testing structured content")
        .tool_fn(
            "get_user",
            "Get user info by ID",
            async |input: GetUserInput, _context| {
                Ok(UserInfo {
                    name: format!("User{}", input.id),
                    age: 20 + input.id,
                })
            },
            sacp::tool_fn!(),
        )
        .build();

    Ok(sacp::DynComponent::new(StructuredProxy { mcp_server }))
}

struct StructuredProxy<R: sacp::JrResponder<ProxyToConductor>> {
    mcp_server: McpServer<ProxyToConductor, R>,
}

impl<R: sacp::JrResponder<ProxyToConductor> + 'static + Send> Component<ProxyToConductor>
    for StructuredProxy<R>
{
    async fn serve(
        self,
        client: impl Component<sacp::link::ConductorToProxy>,
    ) -> Result<(), sacp::Error> {
        ProxyToConductor::builder()
            .name("structured-proxy")
            .with_mcp_server(self.mcp_server)
            .serve(client)
            .await
    }
}

fn conductor_with_structured() -> impl Component<AgentToClient> {
    Conductor::new_agent(
        "test-conductor".to_string(),
        ProxiesAndAgent::new(TestRhaiAgent).proxy(create_structured_proxy().unwrap()),
        Default::default(),
    )
}

/// Test structured content: tool returns a struct, result has `structured_content`
#[tokio::test]
async fn test_structured_content_returns_object() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
/// This test documents that primitive return types (like i64) go through the unstructured
/// path where the value is serialized to text, then parsed back as JSON to preserve types.
#[tokio::test]
async fn test_unstructured_content_preserves_number_types() -> Result<(), sacp::Error> {
    // The add tool returns i64, which uses unstructured content (text serialization)
    // Our extract_tool_result tries to parse the text as JSON to preserve the number type
    let result = yopo::prompt(
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
/// Strings that aren't valid JSON are returned as-is.
#[tokio::test]
async fn test_unstructured_content_returns_string() -> Result<(), sacp::Error> {
    let result = yopo::prompt(
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
