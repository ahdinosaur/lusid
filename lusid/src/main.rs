use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

use lusid::{Cli, get_config, run};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let config = match get_config(&cli).await {
        Ok(c) => c,
        Err(error) => {
            tracing::error!("{error}");
            std::process::exit(1);
        }
    };

    install_tracing(&config.log);

    if let Err(error) = run(cli, config).await {
        tracing::error!("{error}");
        std::process::exit(1);
    }
}

pub fn install_tracing(level: &str) {
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .with_ansi(true)
        .init();
}
