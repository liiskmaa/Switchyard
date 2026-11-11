use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

/// Installs a stderr subscriber. Level comes from `SWITCHYARD_LOG`
/// (`tracing` filter syntax), defaulting to `info`.
pub fn init(format: LogFormat) {
    let filter = EnvFilter::try_from_env("SWITCHYARD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    let result = match format {
        LogFormat::Text => builder.try_init(),
        LogFormat::Json => builder.json().with_current_span(false).try_init(),
    };
    if let Err(err) = result {
        eprintln!("switchyard-core: logging already initialized: {err}");
    }
}
