mod api;
mod auth;
mod config;
mod paths;
mod pricing;
mod research;
mod ui;
mod update;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => ui::run().await,
        Some("--version") => {
            println!("rsc {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("update") => update::run().await,
        Some(_) => anyhow::bail!("usage: rsc [--version | update]"),
    }
}
