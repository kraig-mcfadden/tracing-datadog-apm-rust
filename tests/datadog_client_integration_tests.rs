use tracing_datadog_apm::datadog_client::{Client, ClientConfig, Traces};

#[test]
fn test_create_datadog_client_default() {
    // given
    let client = Client::create_default();

    // when
    client.send_traces(Traces::new()); // no output here, just checking it didn't panic
}

#[test]
fn test_create_datadog_client_with_config() {
    // given
    let config = ClientConfig::new().datadog_agent_host("foo");
    let client = Client::create_with_config(config);

    // when
    client.send_traces(Traces::new()); // no output here, just checking it didn't panic
}

#[test]
fn test_create_datadog_client_with_config_chained_setters() {
    // given
    let client = Client::create_with_config(
        ClientConfig::new()
            .datadog_agent_host("foo")
            .datadog_agent_port(1234),
    );

    // when
    client.send_traces(Traces::new()); // no output here, just checking it didn't panic
}
