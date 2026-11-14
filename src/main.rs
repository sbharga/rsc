mod api;
mod auth;
mod cli;
mod config;
mod export;
mod paths;
mod pricing;
mod research;
mod ui;
mod update;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None => ui::run().await,
        Some("--version") => {
            println!("rsc {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("update") => update::run().await,
        Some("run" | "resume" | "ls" | "show" | "export") => {
            let code = cli::main(&args[1..]).await?;
            std::process::exit(code)
        }
        Some(_) => anyhow::bail!("{}", cli::USAGE),
    }
}
