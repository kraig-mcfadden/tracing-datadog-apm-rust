use super::datadog_client::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::fmt::Debug;
use std::num::NonZeroU64;
use std::ops::Add;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, UNIX_EPOCH};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Metadata, Subscriber};
use tracing_core::span::Current;

thread_local! {
    static CURRENT_SPAN: RefCell<Vec<Id>> = RefCell::new(Vec::new());
}

#[derive(Default)]
pub struct TracingSubscriberDatadogConfig {
    mappings: HashMap<SpanName, (ServiceName, SpanType)>,
}

impl TracingSubscriberDatadogConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_mapping(mut self, key: SpanName, value: (ServiceName, SpanType)) -> Self {
        self.mappings.insert(key, value);
        self
    }
}

pub struct TracingSubscriberDatadog {
    datadog_client: Client,
    mappings: Mutex<HashMap<SpanName, (ServiceName, SpanType)>>,
    span_builders: Mutex<HashMap<Id, SpanBuilder>>,
    span_metadata: Mutex<HashMap<Id, &'static Metadata<'static>>>,
    span_ref_count: Mutex<HashMap<Id, u32>>,
    dd_env: String,
    dd_service: String,
    dd_version: String,
}

impl TracingSubscriberDatadog {
    pub fn new(datadog_client: Client, config: TracingSubscriberDatadogConfig) -> Self {
        Self {
            datadog_client,
            mappings: Mutex::new(config.mappings),
            span_builders: Mutex::new(HashMap::new()),
            span_metadata: Mutex::new(HashMap::new()),
            span_ref_count: Mutex::new(HashMap::new()),
            dd_env: env::var("DD_ENV").unwrap_or_default(),
            dd_service: env::var("DD_SERVICE").unwrap_or_default(),
            dd_version: env::var("DD_VERSION").unwrap_or_default(),
        }
    }

    #[inline]
    fn span_builders(&self) -> Option<MutexGuard<HashMap<Id, SpanBuilder>>> {
        self.span_builders
            .lock()
            .map_err(|e| {
                log::error!("Unable to acquire lock on span builders map; err {}", e);
            })
            .ok()
    }

    #[inline]
    fn put_span_builder(&self, id: Id, span_builder: SpanBuilder) {
        if let Some(mut map) = self.span_builders() {
            map.insert(id, span_builder);
        }
    }

    #[inline]
    fn remove_span_builder(&self, id: &Id) -> Option<SpanBuilder> {
        if let Some(mut map) = self.span_builders() {
            map.remove(id)
        } else {
            None
        }
    }

    #[inline]
    fn span_metadata(&self) -> Option<MutexGuard<HashMap<Id, &'static Metadata<'static>>>> {
        self.span_metadata
            .lock()
            .map_err(|e| {
                log::error!("Unable to acquire lock on span metadata map; err {}", e);
            })
            .ok()
    }

    #[inline]
    fn put_metadata(&self, id: Id, metadata: &'static Metadata<'static>) {
        self.span_metadata().map(|mut map| map.insert(id, metadata));
    }

    #[inline]
    fn remove_metadata(&self, id: &Id) {
        self.span_metadata().map(|mut map| map.remove(id));
    }
}

// This can be used for determining the parent of new spans, for determining
// the current span for formatting events, etc...
#[inline]
fn current_span_id() -> Option<Id> {
    CURRENT_SPAN.with(|stack| stack.borrow().last().map(Id::clone))
}

impl Subscriber for TracingSubscriberDatadog {
    #[inline]
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        match self.mappings.lock() {
            Ok(mappings) => mappings.contains_key(&SpanName(metadata.name())),
            Err(e) => {
                log::error!("Failed to get lock on span name mappings; err {:?}", e);
                false
            }
        }
    }

    #[inline]
    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let mut span_builder = SpanBuilder::default();
        let id = Id::from_non_zero_u64(span_builder.span_id);
        log::debug!("Making new span: {:?} with id {:?}", span, id);

        // set span name, type, and service
        let name = SpanName(span.metadata().name());
        match self.mappings.lock() {
            Ok(mappings) => {
                let (service, span_type) = mappings.get(&name).unwrap(); // safe to unwrap because span name was checked in fn `enabled`
                span_builder.span_type(span_type.clone());
                span_builder.service(*service);
            }
            Err(e) => log::error!("Failed to get lock on span name mappings; err {:?}", e),
        }
        span_builder.name(name);

        // add DD tags
        span_builder.add_meta(SpanMetaKey::Service, self.dd_service.clone());
        span_builder.add_meta(SpanMetaKey::Env, self.dd_env.clone());
        span_builder.add_meta(SpanMetaKey::Version, self.dd_version.clone());

        // set child / parent relationship if applicable
        if let Some(parent_span_id) = current_span_id() {
            log::debug!("Span {:?} is a child of span {:?}", id, parent_span_id);
            if let Some(span_builders_map) = self.span_builders() {
                if let Some(parent_span_builder) = span_builders_map.get(&parent_span_id) {
                    log::debug!(
                        "Setting trace id to {:?} like parent",
                        parent_span_builder.trace_id
                    );
                    span_builder.trace_id(parent_span_builder.trace_id);
                }
            }
            span_builder.parent_id(parent_span_id.into_non_zero_u64());
        }
        span.record(&mut span_builder);

        // store span builder
        self.put_span_builder(id.clone(), span_builder);
        self.put_metadata(id.clone(), span.metadata());
        self.span_ref_count
            .lock()
            .map(|mut ref_counts| {
                ref_counts.insert(id.clone(), 1);
            })
            .ok();

        id
    }

    #[inline]
    fn record(&self, span: &Id, values: &Record<'_>) {
        log::debug!("Record {:?} for span {:?}", values, span);
        if !values.is_empty() {
            match self.span_builders.lock() {
                Ok(mut span_builders_map) => {
                    if let Some(span_builder) = span_builders_map.get_mut(span) {
                        values.record(span_builder);
                    };
                }
                Err(e) => {
                    log::error!("Unable to acquire lock on span builders map; err {}", e);
                }
            }
        }
    }

    #[inline]
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {
        unimplemented!()
    }

    #[inline]
    fn event(&self, event: &Event<'_>) {
        log::debug!("Received event: {:?}", event);
        if event.is_contextual() {
            if let Some(id) = current_span_id() {
                match self.span_builders.lock() {
                    Ok(mut span_builders_map) => {
                        if let Some(span_builder) = span_builders_map.get_mut(&id) {
                            event.record(span_builder);
                        };
                    }
                    Err(e) => {
                        log::error!("Unable to acquire lock on span builders map; err {}", e);
                    }
                }
            };
        } else if let Some(parent_span) = event.parent() {
            match self.span_builders.lock() {
                Ok(mut span_builders_map) => {
                    if let Some(span_builder) = span_builders_map.get_mut(parent_span) {
                        event.record(span_builder);
                    };
                }
                Err(e) => {
                    log::error!("Unable to acquire lock on span builders map; err {}", e);
                }
            }
        }
    }

    #[inline]
    fn enter(&self, id: &Id) {
        CURRENT_SPAN.with(|stack| {
            stack.borrow_mut().push(id.clone());
        })
    }

    #[inline]
    fn exit(&self, id: &Id) {
        CURRENT_SPAN.with(|stack| {
            match stack.borrow_mut().pop() {
                Some(popped_id) => {
                    if popped_id != *id {
                        log::error!("Popped an id which was not the id passed in! Passed in: {:?} - popped: {:?}", id, popped_id);
                    }
                }
                None => {
                    log::error!("Did not exit a span! Passed in: {:?} - popped: N/A", id);
                }
            }

        })
    }

    #[inline]
    fn clone_span(&self, id: &Id) -> Id {
        self.span_ref_count
            .lock()
            .map(|mut ref_counts| {
                if let Some(ref_count) = ref_counts.get_mut(id) {
                    *ref_count += 1;
                } else {
                    log::error!("Could not clone span {:?} as it did not exist in map", id);
                }
            })
            .ok();
        id.clone()
    }

    #[inline]
    fn try_close(&self, id: Id) -> bool {
        log::debug!("Try close span {:?}", id);
        match self.span_ref_count.lock() {
            Ok(mut ref_counts) => {
                let maybe_ref_count = ref_counts.get_mut(&id);
                if let Some(ref_count) = maybe_ref_count {
                    if *ref_count - 1 == 0 {
                        if let Some(span_builder) = self.remove_span_builder(&id) {
                            let traces = vec![vec![span_builder.build()]];
                            log::debug!("Generated traces: {:?}", &traces);
                            self.datadog_client.send_traces(traces);
                        } else {
                            log::error!("Could not find span builder to remove for span {:?}", id);
                        }
                        self.remove_metadata(&id);
                        return true;
                    } else if (*ref_count as i32 - 1) < 0 {
                        log::error!("Error with reference counting! Ref count was at 0 and try_close was called");
                    } else {
                        *ref_count -= 1;
                    }
                } else {
                    log::error!(
                        "Could not try_close span {:?} as it did not exist in map",
                        id
                    );
                }
            }
            Err(e) => log::error!("Failed to acquire lock on ref counts; err {:?}", e),
        }
        false
    }

    #[inline]
    fn current_span(&self) -> Current {
        match current_span_id().and_then(|id| {
            self.span_metadata().and_then(|metadata_map| {
                metadata_map
                    .get(&id)
                    .map(|metadata| Current::new(id.clone(), metadata))
            })
        }) {
            Some(val) => val,
            None => Current::none(),
        }
    }
}

#[derive(Debug)]
enum FieldName {
    TraceId,
    ParentId,
    Resource,
    Start,
    HttpMethod,
    HttpUrl,
    HttpStatusCode,
    ErrorType,
    ErrorMsg,
    ErrorStack,
}

impl FromStr for FieldName {
    type Err = ();

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "trace_id" => Ok(Self::TraceId),
            "parent_id" => Ok(Self::ParentId),
            "resource" => Ok(Self::Resource),
            "start" => Ok(Self::Start),
            "http_method" => Ok(Self::HttpMethod),
            "http_url" => Ok(Self::HttpUrl),
            "http_status_code" => Ok(Self::HttpStatusCode),
            "error_type" => Ok(Self::ErrorType),
            "error_msg" => Ok(Self::ErrorMsg),
            "error_stack" => Ok(Self::ErrorStack),
            _ => Err(()),
        }
    }
}

impl Visit for SpanBuilder {
    #[inline]
    fn record_u64(&mut self, field: &Field, value: u64) {
        let field_name = FieldName::from_str(field.name());
        if field_name.is_err() {
            log::error!("{} is not a valid span field", field.name());
            return;
        }

        match field_name.unwrap() {
            FieldName::TraceId => match NonZeroU64::new(value) {
                Some(trace_id) => {
                    self.trace_id(trace_id);
                }
                None => log::error!("Invalid trace id; it was zero"),
            },
            FieldName::ParentId => match NonZeroU64::new(value) {
                Some(parent_id) => {
                    self.parent_id(parent_id);
                }
                None => log::error!("Invalid parent id; it was zero"),
            },
            FieldName::HttpStatusCode => {
                self.add_meta(SpanMetaKey::HttpStatusCode, value.to_string());
            }
            FieldName::Start => {
                self.start(UNIX_EPOCH.add(Duration::from_nanos(value)));
            }
            _ => {}
        };
    }

    #[inline]
    fn record_str(&mut self, field: &Field, value: &str) {
        let field_name = FieldName::from_str(field.name());
        if field_name.is_err() {
            log::error!("{} is not a valid span field", field.name());
            return;
        }

        match field_name.unwrap() {
            FieldName::TraceId => match NonZeroU64::from_str(value) {
                Ok(trace_id) => {
                    self.trace_id(trace_id);
                }
                Err(e) => log::error!("Failed parsing trace_id: {:?}", e),
            },
            FieldName::ParentId => match NonZeroU64::from_str(value) {
                Ok(parent_id) => {
                    self.parent_id(parent_id);
                }
                Err(e) => log::error!("Failed parsing parent_id: {:?}", e),
            },
            FieldName::Resource => {
                self.resource(String::from(value));
            }
            FieldName::HttpMethod => {
                self.add_meta(SpanMetaKey::HttpMethod, value);
            }
            FieldName::HttpStatusCode => {
                self.add_meta(SpanMetaKey::HttpStatusCode, value);
            }
            FieldName::HttpUrl => {
                self.add_meta(SpanMetaKey::HttpUrl, value);
            }
            FieldName::ErrorType => {
                self.error(true);
                self.add_meta(SpanMetaKey::ErrorType, value);
            }
            FieldName::ErrorMsg => {
                self.error(true);
                self.add_meta(SpanMetaKey::ErrorMsg, value);
            }
            FieldName::ErrorStack => {
                self.error(true);
                self.add_meta(SpanMetaKey::ErrorStack, value);
            }
            _ => {}
        }
    }

    #[inline]
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.record_str(field, &format!("{:?}", value))
    }
}
