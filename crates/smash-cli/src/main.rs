mod cookbook;

use anyhow::Result;
use clap::Parser;
use cookbook::run_cookbook;
use smash_shell::crossterm::event::KeyEventKind;
use smash_shell::prelude::*;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Launch the smash-shell component gallery
    #[arg(long)]
    cookbook: bool,
}

fn is_ctrl_c_press(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
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
        let quit_dialog = use_dialog(
            "quit smash shell?",
            "Press Ctrl+C again to quit immediately, or choose stay to keep the shell open.",
        );
        quit_dialog.set_labels("stay", "quit");

        while window.update().expect("window update failed") {
            for key in window.key_events.clone() {
                if quit_dialog.is_open() {
                    if is_ctrl_c_press(key) {
                        window.should_quit = true;
                        continue;
                    }

                    match quit_dialog.handle_smash_event(&SmashEvent::Key(key)) {
                        DialogEvent::Confirmed => {
                            window.should_quit = true;
                        }
                        DialogEvent::Cancelled | DialogEvent::Handled | DialogEvent::Ignored => {}
                    }
                    continue;
                }

                if is_ctrl_c_press(key) {
                    quit_dialog.open();
                } else if key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Char('q')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    window.should_quit = true;
                }
            }

            let theme = window.theme;
            window
                .draw(|frame| {
                    let layout = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(5), Constraint::Min(0)])
                        .split(frame.area());

                    frame.render_widget(
                        Paragraph::new("smash shell")
                            .alignment(Alignment::Center)
                            .style(Style::default().add_modifier(Modifier::BOLD)),
                        layout[0],
                    );

                    let instructions = Paragraph::new(
                        "ctrl+q quits • ctrl+c asks once before quitting • --cookbook opens the component gallery",
                    )
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .title("instructions"),
                    );
                    frame.render_widget(instructions, layout[1]);

                    quit_dialog.render(frame, frame.area(), &theme);
                })
                .expect("draw failed");
        }

        window.close().expect("failed to close window");
    });

    main_result
}
