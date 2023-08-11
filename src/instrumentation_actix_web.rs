use actix_web::body::MessageBody;
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::Error;
use futures::future::{ok, Ready};
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::instrument;

type PinnedBoxedFuture<T> = Pin<Box<dyn Future<Output = T>>>;

#[derive(Debug)]
pub struct ActixDatadogTracer;

impl<S, B> Transform<S, ServiceRequest> for ActixDatadogTracer
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static + MessageBody,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = ActixDatadogTracerMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(ActixDatadogTracerMiddleware { service })
    }
}

pub struct ActixDatadogTracerMiddleware<S> {
    service: S,
}

impl<S> Debug for ActixDatadogTracerMiddleware<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActixDatadogTracerMiddleware")
    }
}

impl<S, B> Service<ServiceRequest> for ActixDatadogTracerMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static + MessageBody,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = PinnedBoxedFuture<Result<Self::Response, Self::Error>>;

    actix_service::forward_ready!(service);

    #[instrument(
        name = "request",
        skip_all,
        fields(
            trace_id,
            parent_id,
            resource,
            start,
            http_method,
            http_url,
            http_status_code,
            error_type,
            error_msg,
            error_stack
        )
    )]
    fn call(&self, req: ServiceRequest) -> Self::Future {
        let recordable_data = extract_recordable_data(&req);

        let fut = self.service.call(req);

        Box::pin(async move {
            let current_span = tracing::Span::current();
            current_span.record(
                "resource",
                &*format!("{} {}", recordable_data.method, recordable_data.resource),
            );
            current_span.record("http.method", &*recordable_data.method);
            current_span.record("http.url", &*recordable_data.url);

            if let Some(start) = recordable_data.maybe_start {
                current_span.record("start", start);
            }
            if let Some(trace_id) = recordable_data.maybe_trace_id {
                current_span.record("trace_id", trace_id);
            }
            if let Some(parent_id) = recordable_data.maybe_parent_id {
                current_span.record("parent_id", parent_id);
            }

            let res = fut.await?;

            let current_span = tracing::Span::current();
            current_span.record("http.status_code", res.status().as_str());

            if res.status().is_server_error() {
                current_span.record(
                    "error.msg",
                    &*format!(
                        "Request has failed with HTTP error: {}",
                        res.status().as_str()
                    ),
                );
                if let Some(err) = res.response().error() {
                    current_span.record("error.type", &*format!("{:?}", err));
                    current_span.record("error.stack", &*format!("{:?}", err));
                } else {
                    current_span.record("error.type", "Server side error");
                }
            }

            Ok(res)
        })
    }
}

struct RecordableData {
    maybe_start: Option<u64>,
    resource: String,
    method: String,
    url: String,
    maybe_trace_id: Option<u64>,
    maybe_parent_id: Option<u64>,
}

#[inline]
fn extract_recordable_data(req: &ServiceRequest) -> RecordableData {
    let (maybe_trace_id, maybe_parent_id) = extract_trace_and_parent(req);
    RecordableData {
        maybe_start: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_nanos() as u64),
        resource: req.match_pattern().unwrap_or_else(|| String::from("404")),
        method: req.method().to_string(),
        url: req.uri().to_string(),
        maybe_trace_id,
        maybe_parent_id,
    }
}

#[inline]
fn extract_trace_and_parent(req: &ServiceRequest) -> (Option<u64>, Option<u64>) {
    let mut maybe_trace_id = None;
    let mut maybe_parent_id = None;
    req.headers().iter().for_each(|(key, value)| {
        match &*key.as_str().trim().to_lowercase() {
            // Datadog headers
            "x-datadog-trace-id" => value.to_str().map(|s| maybe_trace_id = Some(s)),
            "x-datadog-parent-id" => value.to_str().map(|s| maybe_parent_id = Some(s)),

            // Zipkin B3 headers
            "x-b3-traceid" => value.to_str().map(|s| maybe_trace_id = Some(s)),
            "x-b3-spanid" => value.to_str().map(|s| maybe_parent_id = Some(s)),

            // B3 single header
            "b3" => {
                // b3: {TraceId}-{SpanId}-{SamplingState}-{ParentSpanId}
                value.to_str().map(|s| {
                    let parts: Vec<&str> = s.split('-').collect();
                    if parts.len() >= 2 {
                        maybe_trace_id = Some(parts[0]);
                        maybe_parent_id = Some(parts[1]);
                    }
                })
            }

            _ => Ok(()),
        }
        .ok();
    });
    (
        maybe_trace_id.and_then(|s| u64::from_str(s).ok()),
        maybe_parent_id.and_then(|s| u64::from_str(s).ok()),
    )
}
