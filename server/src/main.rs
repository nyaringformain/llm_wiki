use std::net::SocketAddr;

use llm_wiki_server::{serve, AppState};

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:19828";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let state = AppState::from_env().await?;
    let addr: SocketAddr = DEFAULT_BIND_ADDR.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("LLM Wiki Personal Server listening on http://{addr}");
    serve(listener, state).await?;

    Ok(())
}
