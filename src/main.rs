use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lanio=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().without_time())
        .init();

    lanio::run(None).await
}
