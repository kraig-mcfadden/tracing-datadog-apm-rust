# Tracing - Datadog Application Performance Monitoring (APM)
`tracing-datadog-apm` uses the [tracing](https://github.com/tokio-rs/tracing) crate
to produce application traces in a format usable by Datadog's APM service.

From the [tracing](https://github.com/tokio-rs/tracing) README:

> `tracing` is a framework for instrumenting Rust programs to collect
> structured, event-based diagnostic information. `tracing` is maintained by the
> Tokio project, but does _not_ require the `tokio` runtime to be used.

By using the [tracing](https://github.com/tokio-rs/tracing) crate
we are able to produce structured trace information in asynchronous and in
multi-threaded environments, which are exactly the kinds of environments
typically being monitored with Datadog APM.

## Installation
Add this line to your `Cargo.toml`:
```toml
tracing-datadog-apm = "0.0.1"
```

That will provide an implementation of `tracing::Subscriber` that will pick up 
trace data, as well as a Datadog client equipped to pass trace data to the 
Datadog agent running on the machine.

If you want APM instrumentation out of the box, this crate aims to provide a few
common ones.  Each instrumentation can be enabled as a feature.  Right now the crate
provides:

`actix-web` middleware
```toml
tracing-datadog-apm = { version = "0.0.1", features = ["actix-web"] }
```

## Usage
### 1) Setup Datadog Agent
First, make sure your application also has a Datadog agent running in the background.
See [more info here](https://docs.datadoghq.com/agent/).

### 2) Create Datadog client
Then, in your code, create a Datadog client. If you're using the latest version of the
Datadog agent, and the agent is running on the same machine as your application,
you can use `.create_default()` to make a client with the correct values.
```rust
let datadog_client = tracing_datadog_apm::datadog_client::Client::create_default();
```

If your Datadog agent is running on another machine or on a different port than the default
8126, you can provide your own config by using `.create_with_config()` and passing in an instance
of `ClientConfig`. `ClientConfig` lets you chain method calls to set the fields you want.
```rust
let datadog_client = tracing_datadog_apm::datadog_client::Client::create_with_config(
    tracing_datadog_apm::datadog_client::ClientConfig::new()
        .datadog_agent_host("foo")
        .datadog_agent_port(1234),
);
```

### 3) Create Datadog tracing `Subscriber`
Next, create a Datadog tracing `Subscriber`. This will take ownership of the Datadog
client created in the previous step. It will also take a `TracingSubscriberDatadogConfig`,
which maps span names to service names and span type. 

The span name is the name of the span according to the `tracing` library. 
The service name and span type are properties of the span when it gets into Datadog 
(notice that both `tracing` and Datadog have a notion of a span).
```rust
use tracing_datadog_apm::subscriber::{TracingSubscriberDatadog, TracingSubscriberDatadogConfig};

let datadog_tracing_subscriber = TracingSubscriberDatadog::new(
    datadog_client,
    TracingSubscriberDatadogConfig::new()
        .add_mapping(
            SpanName("http.request"),
            (ServiceName("my-service-rest"), SpanType::Web),
        )
        .add_mapping(
            SpanName("database.query"),
            (ServiceName("my-service-database"), SpanType::Db),
        )
        .add_mapping(
            SpanName("redis.request"),
            (ServiceName("my-service-cache"), SpanType::Cache),
        )
        .add_mapping(
            SpanName("custom.function"),
            (ServiceName("my-service-function"), SpanType::Custom),
        ),
);
```

There are 4 available `SpanType`s defined by Datadog: `Web`, `Db`, `Cache`, and
`Custom`.

Note that each service name provided will show up as a separate service in the
[APM dashboard](https://app.datadoghq.com/apm), and any time a trace from one
service touches multiple services, each service will show up in the latency breakdown
graph with a percentage of time spent in that service.

Example: See the `% of Time Spent by Service` graph in the lower right.
![Datadog example graphs](https://datadog-docs.imgix.net/images/tracing/visualization/service/out_of_the_box_service_graph.b7b972a596bbec97c174fcd132cb2539.png?fit=max&auto=format&w=1792&h=1009&dpr=2)

You can also map multiple span names and span types to the same service name. In that case that service will show
up in APM, but it will have a dropdown that allows you to select the spans you want to see displayed.

### 4) Set the Datadog Subscriber as the global subscriber
In your application, you will need to be using the [tracing](https://github.com/tokio-rs/tracing)
crate. You should set the Datadog `Subscriber` as the global subscriber (in future
iterations it will also be available as a `Layer` so it can be combined with other
`Subscriber`s).

```rust
tracing::subscriber::set_global_default(datadog_tracing_subscriber)
        .expect("Setting tracing default failed");
```

### 5) Instrument everything
The easiest way to instrument things in an async, multi-threaded environment is 
to use the `tracing` `instrument` attribute macro.

```rust
#[instrument(name = "call.other_service", skip_all, fields(resource, http_method, http_url, http_status_code, error_msg))]
async fn call_other_service(user_uuid: uuid::Uuid) -> bool {
    if let Ok(host) = env::fetch("OTHER_SERVICE_HOST") {
        // define the URL and method for our REST request
        let url = format!("{}/users/{}", host, user_uuid.to_hyphenated());
        let method = "GET";

        // Record some data in the current span about this request:
        // (the current span being a `call.other_service` span created 
        // when we enter this function, thanks to that `instrument` tag)
        //
        // - `resource` is telling us what specifically is being hit in this span.
        // It will show up (along with the other `resource` types) on the APM 
        // dashboard for spans of the type `call.other_service`
        //
        // - `http.method` and `http.url` are just some metadata we're collecting
        //
        let current_span = tracing::Span::current();
        current_span.record("resource", &*format!("{} {}", method, "Other Service"));
        current_span.record("http_method", &*method);
        current_span.record("http_url", &*url);

        // Make the REST request (this stuff is arbitrary, I'm just using
        // reqwest as an example)
        let client = reqwest::Client::new();
        match client.get(&url).send().await {
            Ok(resp) => {
                // Record the response status code as some more metadata
                let current_span = tracing::Span::current();
                current_span.record("http.status_code", resp.status().as_str());
                resp.status().is_success()
            }
            Err(e) => {
                // Alternately, if the request fails, record the error message
                let err_msg = format!("Could not call other service with error: {}", e);
                let current_span = tracing::Span::current();
                current_span.record("error.msg", &*err_msg);
                log::error!("{}", err_msg);
                false
            }
        }
    } else {
        // Record an error message if we failed getting the env var
        let err_msg = "Failed to fetch OTHER_SERVICE_HOST env var";
        let current_span = tracing::Span::current();
        current_span.record("error.msg", err_msg);
        log::error!("{}", err_msg);
        false
    }
}
```

To `instrument` properly, pick a name for the span (this will map to a service and
span type, as defined in the `TracingDatadogSubscriberConfig` - if it's not set in the
config, it will be ignored).  Then `skip` the function args (they will be passed
along by default, unless you skip them). Then define what fields you would actually like to pass along.

The acceptable field names are:
* `trace_id` - the id of the current trace - normally does not need to be passed explicitly
* `parent_id` - the id of the parent span - normally does not need to be passed explicitly
* `resource` - resource name within the given span
* `start` - the start time in nanos from the Unix epoch - normally doesn't need to be passed explicitly
* `http_method` - metadata for http requests (in or out)
* `http_url` - metadata for http requests (in or out)
* `http_status_code` - metadata for http requests (in or out)
* `error_type` - type of the error that occurred (a string)
* `error_msg` - accompanying error message
* `error_stack` - the whole error stack if you have it as a string

At bare minimum, all spans should have a `resource`. For `Web` spans this is easy:
what's the resource for the REST request?  For a `Db` span it is usually the
SQL query but with placeholder values, i.e. `SELECT $1 FROM table WHERE id = $2;`.
`Cache` spans will also often have a query that can serve as the `resource`.
`Custom` spans can do whatever they'd like.

`Web` spans will also typically include the multiple `http` and `error` parameters.

The other span types can make use of the `error` parameters if they need.

For more information on spans, check out 
[these docs](https://tracing-rs.netlify.app/tracing/index.html#spans) 
and for the `instrument` attribute macro, 
[look here](https://tracing-rs.netlify.app/tracing/attr.instrument.html).

For more information on Datadog's treatment of traces and spans,
refer to their [documentation on tracing](https://docs.datadoghq.com/tracing/guide).
