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
                    KeyCode::Esc => {
                        dashboard.unfocus();
                        return false;
                    }
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
                use crate::dashboard::{FocusPanel, FocusState, NavDirection};

                match dashboard.focus_state {
                    FocusState::PanelSelect => {
                        // Arrow keys / hjkl move between panels
                        match code {
                            KeyCode::Up | KeyCode::Char('k') => dashboard.move_focus(NavDirection::Up),
                            KeyCode::Down | KeyCode::Char('j') => dashboard.move_focus(NavDirection::Down),
                            KeyCode::Left | KeyCode::Char('h') => dashboard.move_focus(NavDirection::Left),
                            KeyCode::Right | KeyCode::Char('l') => dashboard.move_focus(NavDirection::Right),
                            KeyCode::Enter => {
                                dashboard.focus_state = FocusState::PanelFocused;
                            }
                            KeyCode::Esc => {
                                dashboard.unfocus();
                            }
                            _ => {}
                        }
                    }
                    FocusState::PanelFocused => {
                        match dashboard.focus_panel {
                            FocusPanel::Terminal => {
                                if code == KeyCode::Esc {
                                    dashboard.focus_state = FocusState::PanelSelect;
                                    return false;
                                }
                                // All keys go to SSH
                                let bytes = key_to_bytes(code, modifiers);
                                if !bytes.is_empty() {
                                    rt.block_on(dashboard.send_input(bytes));
                                }
                            }
                            FocusPanel::Sidebar => {
                                let session = dashboard.sessions[idx].clone();

                                // Check which sub-mode we're in
                                let (in_search, in_goto) = rt.block_on(async {
                                    let data = session.lock().await;
                                    (data.file_browser.search_mode, data.file_browser.goto_mode)
                                });

                                if in_search {
                                    // === Search mode ===
                                    match code {
                                        KeyCode::Esc => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.exit_search();
                                            });
                                        }
                                        KeyCode::Enter => {
                                            // Jump to first match, exit search
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                let query = data.file_browser.search_query.to_lowercase();
                                                if !query.is_empty() {
                                                    if let Some(pos) = data.file_browser.entries.iter().position(|e| {
                                                        e.name.to_lowercase().contains(&query)
                                                    }) {
                                                        data.file_browser.selected = pos;
                                                    }
                                                }
                                                data.file_browser.exit_search();
                                            });
                                        }
                                        KeyCode::Backspace => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.search_query.pop();
                                            });
                                        }
                                        KeyCode::Char(c) => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.search_query.push(c);
                                            });
                                        }
                                        _ => {}
                                    }
                                } else if in_goto {
                                    // === Goto mode with autocomplete ===
                                    match code {
                                        KeyCode::Esc => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.exit_goto();
                                            });
                                        }
                                        KeyCode::Enter => {
                                            // Navigate to the typed path
                                            rt.block_on(async {
                                                let (sftp_arc, goto_path) = {
                                                    let data = session.lock().await;
                                                    (data.sftp.clone(), data.file_browser.goto_path.clone())
                                                };
                                                if let Some(sftp) = sftp_arc {
                                                    let sftp_guard = sftp.lock().await;
                                                    let mut data = session.lock().await;
                                                    let path = goto_path.trim_end_matches('/').to_string();
                                                    if !path.is_empty() {
                                                        data.file_browser.current_path = path;
                                                        crate::file_browser::refresh_listing(&sftp_guard, &mut data.file_browser).await;
                                                    }
                                                    data.file_browser.exit_goto();
                                                }
                                            });
                                        }
                                        KeyCode::Tab => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.autocomplete_selected();
                                                // Refresh suggestions after autocomplete
                                                let sftp_arc = data.sftp.clone();
                                                drop(data);
                                                if let Some(sftp) = sftp_arc {
                                                    let sftp_guard = sftp.lock().await;
                                                    let mut data = session.lock().await;
                                                    crate::file_browser::fetch_goto_suggestions(&sftp_guard, &mut data.file_browser).await;
                                                }
                                            });
                                        }
                                        KeyCode::Up => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                if data.file_browser.goto_selected > 0 {
                                                    data.file_browser.goto_selected -= 1;
                                                }
                                            });
                                        }
                                        KeyCode::Down => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                let len = data.file_browser.goto_suggestions.len();
                                                if data.file_browser.goto_selected + 1 < len {
                                                    data.file_browser.goto_selected += 1;
                                                }
                                            });
                                        }
                                        KeyCode::Backspace => {
                                            rt.block_on(async {
                                                {
                                                    let mut data = session.lock().await;
                                                    data.file_browser.goto_path.pop();
                                                }
                                                let sftp_arc = {
                                                    let data = session.lock().await;
                                                    data.sftp.clone()
                                                };
                                                if let Some(sftp) = sftp_arc {
                                                    let sftp_guard = sftp.lock().await;
                                                    let mut data = session.lock().await;
                                                    crate::file_browser::fetch_goto_suggestions(&sftp_guard, &mut data.file_browser).await;
                                                }
                                            });
                                        }
                                        KeyCode::Char(c) => {
                                            rt.block_on(async {
                                                {
                                                    let mut data = session.lock().await;
                                                    data.file_browser.goto_path.push(c);
                                                }
                                                let sftp_arc = {
                                                    let data = session.lock().await;
                                                    data.sftp.clone()
                                                };
                                                if let Some(sftp) = sftp_arc {
                                                    let sftp_guard = sftp.lock().await;
                                                    let mut data = session.lock().await;
                                                    crate::file_browser::fetch_goto_suggestions(&sftp_guard, &mut data.file_browser).await;
                                                }
                                            });
                                        }
                                        _ => {}
                                    }
                                } else {
                                    // === Normal sidebar mode ===
                                    match code {
                                        KeyCode::Esc => {
                                            dashboard.focus_state = FocusState::PanelSelect;
                                        }
                                        KeyCode::Char('/') => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.enter_search();
                                            });
                                        }
                                        KeyCode::Char('g') => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                data.file_browser.enter_goto();
                                            });
                                        }
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                if data.file_browser.selected > 0 {
                                                    data.file_browser.selected -= 1;
                                                }
                                            });
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            rt.block_on(async {
                                                let mut data = session.lock().await;
                                                let len = data.file_browser.entries.len();
                                                if data.file_browser.selected + 1 < len {
                                                    data.file_browser.selected += 1;
                                                }
                                            });
                                        }
                                        KeyCode::Enter | KeyCode::Right => {
                                            // Enter directory or view file
                                            rt.block_on(async {
                                                let (sftp_arc, selected, is_dir) = {
                                                    let data = session.lock().await;
                                                    let sftp = data.sftp.clone();
                                                    let sel = data.file_browser.selected;
                                                    let dir = data.file_browser.entries.get(sel).map(|e| e.is_dir).unwrap_or(false);
                                                    (sftp, sel, dir)
                                                };
                                                if let Some(sftp) = sftp_arc {
                                                    let sftp_guard = sftp.lock().await;
                                                    let mut data = session.lock().await;
                                                    if is_dir {
                                                        crate::file_browser::enter_directory(&sftp_guard, &mut data.file_browser, selected).await;
                                                    } else {
                                                        crate::file_browser::read_file(&sftp_guard, &mut data.file_browser, selected).await;
                                                    }
                                                }
                                            });
                                        }
                                        KeyCode::Left | KeyCode::Backspace => {
                                            // Go up one directory / close file view
                                            rt.block_on(async {
                                                let is_viewing_file = {
                                                    let data = session.lock().await;
                                                    data.file_browser.viewing_file.is_some()
                                                };
                                                if is_viewing_file {
                                                    let mut data = session.lock().await;
                                                    data.file_browser.close_file();
                                                } else {
                                                    let sftp_arc = {
                                                        let data = session.lock().await;
                                                        data.sftp.clone()
                                                    };
                                                    if let Some(sftp) = sftp_arc {
                                                        let sftp_guard = sftp.lock().await;
                                                        let mut data = session.lock().await;
                                                        crate::file_browser::go_up(&sftp_guard, &mut data.file_browser).await;
                                                    }
                                                }
                                            });
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            // View-only panels: only Esc returns to PanelSelect
                            FocusPanel::Disk | FocusPanel::SysInfo | FocusPanel::Main => {
                                if code == KeyCode::Esc {
                                    dashboard.focus_state = FocusState::PanelSelect;
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                if code == KeyCode::Esc {
                    dashboard.unfocus();
                    return false;
                }
            }
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
