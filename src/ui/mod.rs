mod app;
mod draw;
pub(crate) mod fmt;

use crate::config::Config;
use crate::pricing::Pricing;
use crate::research::store;
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc::unbounded_channel;

pub async fn run() -> anyhow::Result<()> {
    crate::paths::ensure_dirs()?;
    let (cfg, cfg_err) = Config::load();
    let pricing = Pricing::load();
    crate::research::prompts::seed();
    store::mark_interrupted();

    let (msg_tx, mut msg_rx) = unbounded_channel();
    let (run_tx, mut run_rx) = unbounded_channel();
    let mut app = app::App::new(cfg, cfg_err, pricing, msg_tx, run_tx);

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    let result: anyhow::Result<()> = async {
        loop {
            terminal.draw(|f| draw::draw(f, &mut app))?;
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(Event::Key(k))) => app.on_key(k),
                    Some(Ok(Event::Paste(s))) => app.on_paste(s),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(m) = msg_rx.recv() => app.on_msg(m),
                Some(e) = run_rx.recv() => app.on_run_event(e),
                _ = tick.tick() => app.on_tick(),
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    result
}
