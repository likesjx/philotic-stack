use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    membrane_telegram::run().await
}
