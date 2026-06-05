#[tokio::main]
async fn main() -> anyhow::Result<()> {
    office_automate_server::run_cli().await
}
