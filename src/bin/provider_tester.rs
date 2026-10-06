//! PhoenixAgent Provider Tester CLI
//!
//! Usage:
//!   phoenix-test list                    # List all providers
//!   phoenix-test models <provider>      # List models for a provider

use clap::{Parser, Subcommand};
use phoenix_agent::providers::providers_data;

#[derive(Parser)]
#[command(name = "phoenix-test")]
#[command(about = "PhoenixAgent Provider Tester - Test LLM providers")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all available providers
    List,
    /// List all models for a provider
    Models {
        /// Provider ID (e.g., openai, anthropic, deepseek)
        provider: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List => {
            println!(
                "\n📦 Available Providers ({} total)\n",
                providers_data::all_providers().len()
            );
            println!("{:<20} {:<30} {:<50}", "ID", "Name", "Base URL");
            println!("{}", "-".repeat(100));

            let mut providers = providers_data::all_providers();
            providers.sort_by(|a, b| a.id.cmp(b.id));

            for provider in providers {
                println!(
                    "{:<20} {:<30} {:<50}",
                    provider.id, provider.name, provider.base_url
                );
            }
            println!("\n");
        }
        Commands::Models { provider } => {
            if let Some(p) = providers_data::get_provider(&provider) {
                println!(
                    "\n📋 Models for {} ({}) - {} models\n",
                    p.name,
                    p.id,
                    p.models.len()
                );

                for (i, model) in p.models.iter().enumerate() {
                    println!(
                        "  {}. {} (ctx {}, reasoning: {})",
                        i + 1,
                        model.id,
                        model.context_window,
                        model.reasoning
                    );
                }
                println!();
            } else {
                eprintln!("\n❌ Unknown provider: {}\n", provider);
                eprintln!("Use 'phoenix-test list' to see available providers.\n");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
