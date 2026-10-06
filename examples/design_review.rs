//! Run the same native design tool without a model or gateway session.
//! Example: cargo run --example design_review < web/landing/design.json
use std::io::{self, Read};

fn main() -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    io::stdin().take(512 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 512 * 1024, "Design input exceeds 512 KiB");
    let input: phoenix_agent::tools::design_studio::DesignStudioInput = serde_json::from_slice(&bytes)?;
    let output = phoenix_agent::tools::design_studio::execute(input)?;
    println!("{}", output.content);
    Ok(())
}
