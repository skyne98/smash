use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    prelude::*,
    widgets::{Block, Paragraph},
};
use tokio::task;

pub struct App {
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self { should_quit: false }
    }
}

pub fn init() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    let backend = CrosstermBackend::new(std::io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub async fn run(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui(frame, app))?;
        
        if let Some(event) = poll_events().await {
            match event {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q') {
                        app.should_quit = true;
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

async fn poll_events() -> Option<Event> {
    task::spawn_blocking(|| {
        if event::poll(std::time::Duration::from_millis(100)).ok()? {
            event::read().ok()
        } else {
            None
        }
    })
    .await
    .ok()
    .flatten()
}

fn ui(frame: &mut Frame, _app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    let greeting = Paragraph::new("Welcome to Smash Shell!")
        .block(Block::default().title("Smash"));
    frame.render_widget(greeting, layout[0]);

    let status = Paragraph::new("Press 'q' to quit")
        .block(Block::default().title("Status"));
    frame.render_widget(status, layout[1]);
}
