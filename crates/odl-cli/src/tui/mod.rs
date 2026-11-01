//! Terminal UI (placeholder; implemented in a follow-up commit).

use anyhow::Result;
use odl_client::Client;

pub async fn run_standalone(_client: Client, _since: String) -> Result<()> {
    anyhow::bail!("the TUI is not implemented yet")
}
