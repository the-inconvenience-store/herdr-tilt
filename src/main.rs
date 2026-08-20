use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "herdr-tilt", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Open or focus the Tilt status pane.
    Open,
    /// Run the Tilt status TUI.
    Tui,
    /// Run a retained Tilt session.
    Run,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Open => herdr_tilt::open::open_panel_from_env(),
        Commands::Tui | Commands::Run => Ok(()),
    }
}
