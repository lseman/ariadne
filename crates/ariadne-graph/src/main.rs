use anyhow::Result;
use clap::Parser;

mod cli;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = cli::handlers::Cli::parse();
    cli::run(&cli.db, &cli.command)
}
