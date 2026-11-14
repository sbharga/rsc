mod api;
mod auth;
mod cli;
mod config;
mod export;
mod paths;
mod pricing;
mod research;
mod term;
mod ui;
mod uninstall;
mod update;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match dispatch(&args).await {
        Ok(code) => code,
        Err(e) if e.is::<cli::UsageError>() => {
            eprintln!("{}", cli::usage());
            1
        }
        Err(e) => {
            term::report(&e);
            1
        }
    };
    std::process::exit(code)
}

async fn dispatch(args: &[String]) -> anyhow::Result<i32> {
    match args.get(1).map(String::as_str) {
        None => ui::run().await.map(|()| 0),
        Some("--version") => {
            let p = term::out();
            println!("{} {}", p.bold("rsc"), env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some("update") => update::run().await.map(|()| 0),
        Some("uninstall") => uninstall::run(&args[2..]).await,
        Some("run" | "resume" | "ls" | "show" | "export") => cli::main(&args[1..]).await,
        Some(_) => Err(cli::UsageError.into()),
    }
}
