# Changelog
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.1] - 2023-08-10
### Added
- Configurable Datadog agent client with MPSC channel for sending traces
- A `tokio-tracing` `Subscriber` to record trace data in Datadog's APM format
- Optional `actix-web` middleware to trace all requests if you're using the `actix-web` server framework

[0.0.1]: https://github.com/kraig-mcfadden/tracing-datadog-apm-rust/releases/tag/v0.0.1
