use clap::{Parser, Subcommand};
use mediaforge_config::{AppConfig, RuntimeMode};
use std::error::Error;

#[derive(Debug, Parser)]
#[command(name = "mediaforge")]
#[command(about = "MediaForge media processing and delivery platform")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Api,
    Worker,
    Combined,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    let config = AppConfig::from_env()?;
    mediaforge_observability::init_tracing(&config.log_level);

    let command = cli.command.unwrap_or(match config.runtime_mode {
        RuntimeMode::Api => Command::Api,
        RuntimeMode::Worker => Command::Worker,
        RuntimeMode::Combined => Command::Combined,
    });

    match command {
        Command::Api => mediaforge_api::run_api(config).await?,
        Command::Worker => mediaforge_worker::run_worker_loop(config).await?,
        Command::Combined => run_combined(config).await?,
    }

    Ok(())
}

async fn run_combined(config: AppConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let api_config = config.clone();
    let worker_config = config;

    let api_task = tokio::spawn(async move {
        mediaforge_api::run_api(api_config)
            .await
            .map_err(|error| error.to_string())
    });
    let worker_task = tokio::spawn(async move {
        mediaforge_worker::run_worker_loop(worker_config)
            .await
            .map_err(|error| error.to_string())
    });

    tokio::select! {
        result = api_task => {
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(boxed_error(error)),
                Err(error) => Err(Box::new(error) as Box<dyn Error + Send + Sync>),
            }
        }
        result = worker_task => {
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(boxed_error(error)),
                Err(error) => Err(Box::new(error) as Box<dyn Error + Send + Sync>),
            }
        }
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

fn boxed_error(message: String) -> Box<dyn Error + Send + Sync> {
    Box::new(std::io::Error::other(message))
}
