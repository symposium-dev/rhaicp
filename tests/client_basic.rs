use rhaicp::RhaiAgent;
use rhaicp::client::RhaiClient;

#[tokio::test]
async fn script_returns_last_expression() {
    let result = RhaiClient::new()
        .execute(RhaiAgent::new(), "40 + 2")
        .await
        .unwrap();
    assert_eq!(result, "42");
}

#[tokio::test]
async fn script_with_no_expression_returns_unit() {
    let result = RhaiClient::new()
        .execute(RhaiAgent::new(), "let x = 1;")
        .await
        .unwrap();
    assert_eq!(result, "");
}

#[tokio::test]
async fn script_syntax_error() {
    let result = RhaiClient::new()
        .execute(RhaiAgent::new(), "let x = ;")
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn script_runtime_error() {
    let result = RhaiClient::new()
        .execute(RhaiAgent::new(), r#"throw "boom";"#)
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("boom"), "got: {err}");
}

#[tokio::test]
async fn script_division_by_zero() {
    let result = RhaiClient::new().execute(RhaiAgent::new(), "1 / 0").await;
    assert!(result.is_err());
}
