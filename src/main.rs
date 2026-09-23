#[tokio::main]
async fn main() -> anyhow::Result<()> {
    demodex::daemon::run().await
}
