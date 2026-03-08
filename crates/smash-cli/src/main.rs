mod cookbook;

use anyhow::Result;
use clap::Parser;
use smash_shell::prelude::*;
use smash_shell::tui_big_text::{BigTextBuilder, PixelSize};
use cookbook::run_cookbook;
use smash_shell::crossterm::event::KeyEventKind;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Launch the smash-shell cookbook to test all features
    #[arg(long)]
    cookbook: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup panic hook to ensure terminal is restored
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = smash_shell::crossterm::terminal::disable_raw_mode();
        let _ = smash_shell::crossterm::execute!(
            std::io::stdout(),
            smash_shell::crossterm::terminal::LeaveAlternateScreen,
            smash_shell::crossterm::cursor::Show
        );
        original_hook(panic_info);
    }));

    let args = Args::parse();

    if args.cookbook {
        return run_cookbook().await;
    }

    // Initialize reactive root for the main app
    // We run the entire loop inside the root to ensure signals are always accessible
    let mut main_result = Ok(());
    let _root = create_root(|| {
        let mut window = match Window::new() {
            Ok(w) => w,
            Err(e) => {
                main_result = Err(e);
                return;
            }
        };

        while window.update().expect("window update failed") {
            for key in window.key_events.clone() {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    window.should_quit = true;
                }
            }

            window.draw(|frame| {
                let layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(10), Constraint::Min(0)])
                    .split(frame.area());

                let big_text = BigTextBuilder::default()
                    .pixel_size(PixelSize::HalfHeight)
                    .lines(vec!["smash".into(), "shell".into()])
                    .build();
                
                frame.render_widget(big_text, layout[0]);

                let instructions = Paragraph::new("press 'ctrl+q' to quit | use --cookbook to see more")
                    .block(Block::default().borders(Borders::ALL).title("instructions"));
                frame.render_widget(instructions, layout[1]);
            }).expect("draw failed");
        }

        window.close().expect("failed to close window");
    });
    
    main_result
}
