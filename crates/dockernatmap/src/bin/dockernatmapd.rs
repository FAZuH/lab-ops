use color_eyre::Result;
use dockernatmap::daemon;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    daemon::run_daemon().await?;

    Ok(())
}
