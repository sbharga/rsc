mod api;
mod auth;
mod config;
mod paths;
mod pricing;
mod research;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Some(a) = std::env::args().nth(1) {
        if a == "--version" {
            println!("rsc {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        anyhow::bail!("usage: rsc [--version]");
    }
    ui::run().await
}
