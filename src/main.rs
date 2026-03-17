mod dashboard;
mod disk_info;
mod file_browser;
mod metrics;
mod process_info;
mod session;
mod ssh_config;
mod system_info;
mod ui;

use std::io;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use dashboard::Dashboard;
use metrics::MetricType;
use session::SessionState;
use ssh_config::parse_ssh_config;

fn main() -> io::Result<()> {
    // Build a multi-threaded tokio runtime for async SSH sessions
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    // Parse SSH config
    let hosts = parse_ssh_config();
    if hosts.is_empty() {
        eprintln!("No hosts found in ~/.ssh/config");
        return Ok(());
    }

    // Initialize TUI
    let mut terminal = ratatui::init();
    let size = terminal.size().unwrap_or_default();

    // Calculate grid columns based on host count
    let cols = if hosts.len() <= 3 {
        hosts.len().max(1)
    } else if hosts.len() <= 8 {
        3
    } else {
        4
    };

    // Start directly in Dashboard mode
    let mut dashboard = Dashboard::new(hosts, cols, size.width, size.height, rt.handle());

    let result = run_app(&mut terminal, &mut dashboard, &rt);
    ratatui::restore();
    result?;

    println!("Goodbye!");
    Ok(())
}

fn run_app(
    terminal: &mut ratatui::DefaultTerminal,
    dashboard: &mut Dashboard,
    rt: &tokio::runtime::Runtime,
) -> io::Result<()> {
    let mut should_quit = false;

    loop {
        terminal.draw(|frame| ui::draw_dashboard(frame, dashboard, rt.handle()))?;

        if should_quit {
            break;
        }

        // Short poll timeout for responsive UI updates
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                should_quit = handle_input(dashboard, key.code, key.modifiers, rt);
            }
        }
    }
    Ok(())
}

fn handle_input(
    dashboard: &mut Dashboard,
    code: KeyCode,
    modifiers: KeyModifiers,
    rt: &tokio::runtime::Runtime,
) -> bool {
    if let Some(idx) = dashboard.focused {
        // === Focused mode ===
        if code == KeyCode::Esc {
            dashboard.unfocus();
            return false;
        }

        let state = rt.block_on(async {
            let data = dashboard.sessions[idx].lock().await;
            data.state.clone()
        });

        match state {
            SessionState::NeedPassword => {
                if !dashboard.entering_password {
                    dashboard.entering_password = true;
                }
                match code {
                    KeyCode::Enter => {
                        rt.block_on(dashboard.submit_password());
                    }
                    KeyCode::Backspace => {
                        dashboard.password_input.pop();
                    }
                    KeyCode::Char(c) => {
                        dashboard.password_input.push(c);
                    }
                    _ => {}
                }
            }
            SessionState::Connected => {
                let bytes = key_to_bytes(code, modifiers);
                if !bytes.is_empty() {
                    rt.block_on(dashboard.send_input(bytes));
                }
            }
            _ => {}
        }
    } else {
        // === Grid mode ===
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Up | KeyCode::Char('k') => dashboard.move_up(),
            KeyCode::Down | KeyCode::Char('j') => dashboard.move_down(),
            KeyCode::Left | KeyCode::Char('h') => dashboard.move_left(),
            KeyCode::Right | KeyCode::Char('l') => dashboard.move_right(),
            KeyCode::Enter => dashboard.focus(rt.handle()),
            KeyCode::Tab => dashboard.cycle_metric(),
            KeyCode::Char('1') => dashboard.set_metric(MetricType::Cpu),
            KeyCode::Char('2') => dashboard.set_metric(MetricType::Memory),
            KeyCode::Char('3') => dashboard.set_metric(MetricType::Network),
            _ => {}
        }
    }

    false
}

/// Convert a KeyCode to bytes to send to the remote terminal.
fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
    match code {
        KeyCode::Char(c) => {
            if modifiers.contains(KeyModifiers::CONTROL) {
                let byte = (c as u8).wrapping_sub(b'a').wrapping_add(1);
                vec![byte]
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        _ => vec![],
    }
}
