use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::num::NonZeroU64;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// ClientConfig comes with sensible defaults. Calling either ClientConfig::default() or
/// ClientConfig::new() will create a ClientConfig instance with these defaults. If any
/// fields need to be changed, you only need to call the setter for that field - the others
/// will retain their default value.
///
/// If you intend to use entirely default values, you don't need to instantiate a ClientConfig.
/// Instead, you can just call Client::create_default().
pub struct ClientConfig {
    datadog_agent_host: String,
    datadog_agent_port: u32,
    connect_timeout_ms: u64,
    request_timeout_ms: u64,
}

impl ClientConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn datadog_agent_host(mut self, host: impl Into<String>) -> Self {
        self.datadog_agent_host = host.into();
        self
    }

    pub fn datadog_agent_port(mut self, port: u32) -> Self {
        self.datadog_agent_port = port;
        self
    }

    pub fn connect_timeout_ms(mut self, ms: u64) -> Self {
        self.connect_timeout_ms = ms;
        self
    }

    pub fn request_timeout_ms(mut self, ms: u64) -> Self {
        self.request_timeout_ms = ms;
        self
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            datadog_agent_host: String::from("localhost"),
            datadog_agent_port: 8126,
            connect_timeout_ms: 100,
            request_timeout_ms: 100,
        }
    }
}

pub struct Client {
    sender_mutex: Mutex<Sender<serde_json::Value>>,
    _daemon: JoinHandle<()>,
}

impl Client {
    pub fn create_default() -> Self {
        Self::create_with_config(ClientConfig::default())
    }

    pub fn create_with_config(config: ClientConfig) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<serde_json::Value>();

        let daemon: JoinHandle<()> = std::thread::spawn(move || {
            log::info!("Starting daemon thread to pass traces to Datadog agent");
            let client = reqwest::blocking::ClientBuilder::new()
                .connect_timeout(Duration::from_millis(config.connect_timeout_ms))
                .timeout(Duration::from_millis(config.request_timeout_ms))
                .build()
                .map_err(|e| log::error!("Failed to construct client, killing daemon; err {:?}", e))
                .unwrap();
            let dd_agent_url = format!(
                "http://{}:{}/v0.3/traces",
                config.datadog_agent_host, config.datadog_agent_port
            );
            loop {
                match receiver.recv() {
                    Ok(trace_json) => {
                        send_traces_to_datadog_agent(&client, &dd_agent_url, trace_json);
                    }
                    Err(e) => log::error!("Failed to receive traces on mpsc channel; err {:?}", e),
                }
            }
        });

        Self {
            sender_mutex: Mutex::new(sender),
            _daemon: daemon,
        }
    }

    #[inline]
    pub fn send_traces(&self, traces: Traces) {
        let trace_json = serde_json::to_value(traces).unwrap_or_else(|e| {
            log::error!("Failed to serialize traces into JSON value. Err: {}", e);
            serde_json::Value::default()
        });
        match self.sender_mutex.lock() {
            Ok(sender) => match sender.send(trace_json) {
                Ok(_) => {}
                Err(e) => log::error!("Failed to send traces on mpsc channel; err {:?}", e),
            },
            Err(e) => log::error!("Failed to get lock on sender; err {:?}", e),
        }
    }
}

#[inline]
fn send_traces_to_datadog_agent(
    client: &reqwest::blocking::Client,
    dd_agent_url: &str,
    trace_json: serde_json::Value,
) {
    match client.put(dd_agent_url).body(trace_json.to_string()).send() {
        Ok(resp) => log::debug!(
            "Successfully sent trace to Datadog agent; response: {:?}",
            resp
        ),
        Err(e) => log::error!("Failed to send trace to Datadog agent; error: {}", e),
    };
}

pub type Traces = Vec<Trace>;

pub type Trace = Vec<Span>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Span {
    duration: u64,
    error: u32,
    meta: HashMap<String, String>,
    metrics: HashMap<String, u64>,
    name: &'static str,
    parent_id: Option<u64>,
    resource: String,
    service: &'static str,
    span_id: u64,
    start: u64,
    trace_id: u64,
    r#type: &'static str,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SpanType {
    Web,
    Db,
    Cache,
    Custom,
}

impl SpanType {
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match *self {
            SpanType::Web => "web",
            SpanType::Db => "db",
            SpanType::Cache => "cache",
            SpanType::Custom => "custom",
        }
    }
}

impl FromStr for SpanType {
    type Err = ();

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match &*s.trim().to_lowercase() {
            "web" => SpanType::Web,
            "db" => SpanType::Db,
            "cache" => SpanType::Cache,
            "custom" => SpanType::Custom,
            _ => SpanType::Custom,
        })
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ServiceName(pub &'static str);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpanName(pub &'static str);

#[derive(Copy, Clone, Debug)]
pub enum SpanMetaKey {
    Service,
    Env,
    Version,
    HttpMethod,
    HttpUrl,
    HttpStatusCode,
    ErrorType,
    ErrorMsg,
    ErrorStack,
}

impl std::fmt::Display for SpanMetaKey {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Service => f.write_str("service"),
            Self::Env => f.write_str("env"),
            Self::Version => f.write_str("version"),
            Self::HttpMethod => f.write_str("http.method"),
            Self::HttpUrl => f.write_str("http.url"),
            Self::HttpStatusCode => f.write_str("http.status_code"),
            Self::ErrorType => f.write_str("error.type"),
            Self::ErrorMsg => f.write_str("error.msg"),
            Self::ErrorStack => f.write_str("error.stack"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpanBuilder {
    error: bool,
    meta: HashMap<String, String>,
    metrics: HashMap<String, u64>,
    name: SpanName,
    pub parent_id: Option<NonZeroU64>,
    resource: String,
    service: ServiceName,
    pub span_id: NonZeroU64,
    start: SystemTime,
    pub trace_id: NonZeroU64,
    r#type: SpanType,
}

impl Default for SpanBuilder {
    #[inline]
    fn default() -> Self {
        Self {
            error: false,
            meta: HashMap::new(),
            metrics: HashMap::new(),
            name: SpanName(""),
            parent_id: None,
            resource: String::new(),
            service: ServiceName(""),
            span_id: generate_id(),
            start: SystemTime::now(),
            trace_id: generate_id(),
            r#type: SpanType::Custom,
        }
    }
}

impl SpanBuilder {
    #[inline]
    pub fn name(&mut self, name: SpanName) -> &mut Self {
        self.name = name;
        self
    }

    #[inline]
    pub fn service(&mut self, service: ServiceName) -> &mut Self {
        self.service = service;
        self
    }

    #[inline]
    pub fn resource(&mut self, resource: String) -> &mut Self {
        self.resource = resource;
        self
    }

    #[inline]
    pub fn span_type(&mut self, span_type: SpanType) -> &mut Self {
        self.r#type = span_type;
        self
    }

    #[inline]
    pub fn trace_id(&mut self, trace_id: NonZeroU64) -> &mut Self {
        self.trace_id = trace_id;
        self
    }

    #[inline]
    pub fn start(&mut self, start: SystemTime) -> &mut Self {
        self.start = start;
        self
    }

    #[inline]
    pub fn error(&mut self, error: bool) -> &mut Self {
        self.error = error;
        self
    }

    #[inline]
    pub fn add_meta(&mut self, key: SpanMetaKey, value: impl Into<String>) -> &mut Self {
        self.meta.insert(key.to_string(), value.into());
        self
    }

    #[inline]
    pub fn metrics(&mut self, metrics: HashMap<String, u64>) -> &mut Self {
        self.metrics = metrics;
        self
    }

    #[inline]
    pub fn parent_id(&mut self, parent_id: NonZeroU64) -> &mut Self {
        self.parent_id = Some(parent_id);
        self
    }

    #[inline]
    pub fn build(&self) -> Span {
        let duration = SystemTime::now()
            .duration_since(self.start)
            .unwrap_or_else(|_| Duration::from_nanos(0))
            .as_nanos() as u64;
        Span {
            duration,
            error: if self.error { 1 } else { 0 },
            meta: self.meta.clone(),
            metrics: self.metrics.clone(),
            name: self.name.0,
            parent_id: self.parent_id.map(NonZeroU64::get),
            resource: self.resource.clone(),
            service: self.service.0,
            span_id: self.span_id.get(),
            start: self
                .start
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_nanos() as u64,
            trace_id: self.trace_id.get(),
            r#type: self.r#type.as_str(),
        }
    }
}

#[inline]
pub fn generate_id() -> NonZeroU64 {
    rand::thread_rng().gen()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ClientConfig::default();
        assert_eq!(config.datadog_agent_host, "localhost");
        assert_eq!(config.datadog_agent_port, 8126);
        assert_eq!(config.connect_timeout_ms, 100);
        assert_eq!(config.request_timeout_ms, 100);
    }

    #[test]
    fn test_new_config() {
        let config = ClientConfig::new();
        assert_eq!(config.datadog_agent_host, "localhost");
        assert_eq!(config.datadog_agent_port, 8126);
        assert_eq!(config.connect_timeout_ms, 100);
        assert_eq!(config.request_timeout_ms, 100);
    }

    #[test]
    fn test_config_host() {
        let config = ClientConfig::new().datadog_agent_host("foo");
        assert_eq!(config.datadog_agent_host, "foo");
        assert_eq!(config.datadog_agent_port, 8126);
        assert_eq!(config.connect_timeout_ms, 100);
        assert_eq!(config.request_timeout_ms, 100);
    }

    #[test]
    fn test_config_port() {
        let config = ClientConfig::new().datadog_agent_port(1234);
        assert_eq!(config.datadog_agent_host, "localhost");
        assert_eq!(config.datadog_agent_port, 1234);
        assert_eq!(config.connect_timeout_ms, 100);
        assert_eq!(config.request_timeout_ms, 100);
    }

    #[test]
    fn test_config_connect_timeout() {
        let config = ClientConfig::new().connect_timeout_ms(500);
        assert_eq!(config.datadog_agent_host, "localhost");
        assert_eq!(config.datadog_agent_port, 8126);
        assert_eq!(config.connect_timeout_ms, 500);
        assert_eq!(config.request_timeout_ms, 100);
    }

    #[test]
    fn test_config_request_timeout() {
        let config = ClientConfig::new().request_timeout_ms(750);
        assert_eq!(config.datadog_agent_host, "localhost");
        assert_eq!(config.datadog_agent_port, 8126);
        assert_eq!(config.connect_timeout_ms, 100);
        assert_eq!(config.request_timeout_ms, 750);
    }

    #[test]
    fn test_config_chain() {
        let config = ClientConfig::new()
            .datadog_agent_host("foo")
            .datadog_agent_port(1234)
            .connect_timeout_ms(500)
            .request_timeout_ms(750);
        assert_eq!(config.datadog_agent_host, "foo");
        assert_eq!(config.datadog_agent_port, 1234);
        assert_eq!(config.connect_timeout_ms, 500);
        assert_eq!(config.request_timeout_ms, 750);
    }

    #[test]
    fn test_span_type_web() {
        let span_type = SpanType::from_str("web").unwrap();
        assert_eq!(span_type, SpanType::Web);
        assert_eq!(span_type.as_str(), "web");
    }

    #[test]
    fn test_span_type_db() {
        let span_type = SpanType::from_str("db").unwrap();
        assert_eq!(span_type, SpanType::Db);
        assert_eq!(span_type.as_str(), "db");
    }

    #[test]
    fn test_span_type_cache() {
        let span_type = SpanType::from_str("cache").unwrap();
        assert_eq!(span_type, SpanType::Cache);
        assert_eq!(span_type.as_str(), "cache");
    }

    #[test]
    fn test_span_type_custom() {
        let span_type = SpanType::from_str("custom").unwrap();
        assert_eq!(span_type, SpanType::Custom);
        assert_eq!(span_type.as_str(), "custom");
    }

    #[test]
    fn test_span_type_from_str_capitalized() {
        let span_type = SpanType::from_str("Web").unwrap();
        assert_eq!(span_type, SpanType::Web);
        assert_eq!(span_type.as_str(), "web");
    }

    #[test]
    fn test_span_type_other_string_defaults_to_custom() {
        let span_type = SpanType::from_str("fake type").unwrap();
        assert_eq!(span_type, SpanType::Custom);
        assert_eq!(span_type.as_str(), "custom");
    }

    #[test]
    fn test_default_span_builder() {
        let span = SpanBuilder::default().build();
        assert_eq!(span.error, 0);
        assert_eq!(span.meta, HashMap::new());
        assert_eq!(span.metrics, HashMap::new());
        assert_eq!(span.name, "");
        assert_eq!(span.parent_id, None);
        assert_eq!(span.resource, "");
        assert_eq!(span.service, "");
        assert!(span.span_id > 0);
        assert!(span.start > 0);
        assert!(span.trace_id > 0);
        assert_eq!(span.r#type, "custom");
    }

    #[test]
    fn test_span_builder() {
        let start = SystemTime::now()
            .checked_sub(Duration::from_nanos(100))
            .unwrap();
        let parent_id = NonZeroU64::new(5).unwrap();
        let trace_id = NonZeroU64::new(100).unwrap();
        let name = "foo";
        let resource = "bar";
        let service = "aliceandbob";
        let r#type = SpanType::Db;
        let span = SpanBuilder::default()
            .start(start)
            .parent_id(parent_id)
            .trace_id(trace_id)
            .error(true)
            .name(SpanName(name))
            .resource(String::from(resource))
            .service(ServiceName(service))
            .span_type(r#type)
            .build();
        assert!(span.duration > 100);
        assert_eq!(span.error, 1);
        assert_eq!(span.meta, HashMap::new());
        assert_eq!(span.metrics, HashMap::new());
        assert_eq!(span.name, name);
        assert_eq!(span.parent_id, Some(parent_id.get()));
        assert_eq!(span.resource, resource);
        assert_eq!(span.service, service);
        assert!(span.span_id > 0);
        assert_eq!(
            span.start,
            start.duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
        );
        assert_eq!(span.trace_id, trace_id.get());
        assert_eq!(span.r#type, "db");
    }

    #[test]
    fn test_span_meta_key_service() {
        assert_eq!(&*SpanMetaKey::Service.to_string(), "service");
    }

    #[test]
    fn test_span_meta_key_env() {
        assert_eq!(&*SpanMetaKey::Env.to_string(), "env");
    }

    #[test]
    fn test_span_meta_key_version() {
        assert_eq!(&*SpanMetaKey::Version.to_string(), "version");
    }

    #[test]
    fn test_span_meta_key_http_method() {
        assert_eq!(&*SpanMetaKey::HttpMethod.to_string(), "http.method");
    }

    #[test]
    fn test_span_meta_key_http_url() {
        assert_eq!(&*SpanMetaKey::HttpUrl.to_string(), "http.url");
    }

    #[test]
    fn test_span_meta_key_http_status_code() {
        assert_eq!(
            &*SpanMetaKey::HttpStatusCode.to_string(),
            "http.status_code"
        );
    }

    #[test]
    fn test_span_meta_key_error_msg() {
        assert_eq!(&*SpanMetaKey::ErrorMsg.to_string(), "error.msg");
    }

    #[test]
    fn test_span_meta_key_error_stack() {
        assert_eq!(&*SpanMetaKey::ErrorStack.to_string(), "error.stack");
    }

    #[test]
    fn test_span_meta_key_error_type() {
        assert_eq!(&*SpanMetaKey::ErrorType.to_string(), "error.type");
    }
}
