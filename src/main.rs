mod app;
mod clipboard;
mod db;
mod favorites;
mod fuzzy;
mod ui;

use std::str::FromStr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui_cheese::theme::Palette;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::app::App;
use crate::db::Db;

/// A cozy terminal editor for live Postgres tables.
#[derive(Parser)]
#[command(name = "qbench", version)]
struct Cli {
    /// Postgres connection string, e.g. postgres://user:pass@localhost:5432/mydb
    #[arg(env = "DATABASE_URL")]
    url: String,

    /// Rows fetched per page
    #[arg(long, default_value_t = 200)]
    page_size: usize,

    /// Leave the mouse to the terminal (native text selection) instead of qbench
    #[arg(long)]
    no_mouse: bool,

    /// Color theme
    #[arg(long, value_enum, default_value_t = Theme::Charm)]
    theme: Theme,
}

#[derive(Clone, Copy, ValueEnum)]
enum Theme {
    Charm,
    Dark,
    Light,
    Ocean,
    Sunset,
}

impl Theme {
    fn palette(self) -> Palette {
        match self {
            Theme::Charm => Palette::charm(),
            Theme::Dark => Palette::dark(),
            Theme::Light => Palette::light(),
            Theme::Ocean => Palette::ocean(),
            Theme::Sunset => Palette::sunset(),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let opts = PgConnectOptions::from_str(&cli.url)
        .context("invalid connection string")?
        .application_name("qbench");
    let db_name = opts.get_database().unwrap_or(opts.get_username()).to_string();
    let host = match opts.get_socket() {
        Some(_) => "socket".to_string(),
        None => format!("{}:{}", opts.get_host(), opts.get_port()),
    };
    let label = format!("{}@{host}/{db_name}", opts.get_username());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let pool = rt
        .block_on(
            PgPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(Duration::from_secs(10))
                .connect_with(opts),
        )
        .with_context(|| format!("could not connect to {label}"))?;

    let (tx, rx) = mpsc::channel();
    let db = Db::new(pool, rt.handle().clone(), tx);
    let mut app = App::new(db, cli.theme.palette(), label.clone(), label, cli.page_size.max(1));

    let mut terminal = ratatui::init();
    if !cli.no_mouse {
        execute!(std::io::stdout(), EnableMouseCapture)?;
        // ratatui's panic hook restores the screen but not mouse reporting.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(std::io::stdout(), DisableMouseCapture);
            hook(info);
        }));
    }
    let result = run(&mut terminal, &mut app, &rx);
    if !cli.no_mouse {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, rx: &mpsc::Receiver<db::Response>) -> Result<()> {
    let mut last = Instant::now();
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(60))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                Event::Mouse(m) => app.on_mouse(m),
                _ => {}
            }
        }
        while let Ok(resp) = rx.try_recv() {
            app.on_response(resp);
        }
        let now = Instant::now();
        app.tick(now - last);
        last = now;
    }
    Ok(())
}
