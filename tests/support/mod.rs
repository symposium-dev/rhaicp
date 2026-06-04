use agent_client_protocol::schema::{InitializeRequest, ProtocolVersion};
use agent_client_protocol::{Client, ConnectTo, ConnectionTo};

pub async fn prompt(
    component: impl ConnectTo<Client>,
    prompt_text: &str,
) -> Result<String, agent_client_protocol::Error> {
    let prompt_text = prompt_text.to_string();

    Client
        .builder()
        .name("test-client")
        .connect_with(
            component,
            async move |cx: ConnectionTo<agent_client_protocol::Agent>| {
                cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST))
                    .block_task()
                    .await?;

                let mut session = cx.build_session_cwd()?.block_task().start_session().await?;

                session.send_prompt(&prompt_text)?;
                let result = session.read_to_string().await?;
                Ok(result)
            },
        )
        .await
}
