use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Sparkline, Wrap},
    Frame,
};

use crate::dashboard::{Dashboard, FocusPanel, FocusState};
use crate::metrics::MetricType;
use crate::session::SessionState;

/// Draw the dashboard view.
pub fn draw(frame: &mut Frame, dashboard: &Dashboard, rt: &tokio::runtime::Handle) {
    match dashboard.focused {
        Some(idx) => draw_focused(frame, dashboard, idx, rt),
        None => draw_grid(frame, dashboard, rt),
    }
}

fn draw_grid(frame: &mut Frame, dashboard: &Dashboard, rt: &tokio::runtime::Handle) {
    let area = frame.area();

    // Layout: grid + status bar
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let grid_area = main_chunks[0];
    let status_area = main_chunks[1];

    let rows = dashboard.rows();
    let cols = dashboard.cols;

    // Create row constraints
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();

    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(grid_area);

    for row in 0..rows {
        let items_in_row = if (row + 1) * cols <= dashboard.total() {
            cols
        } else {
            dashboard.total() - row * cols
        };

        let col_constraints: Vec<Constraint> = (0..items_in_row)
            .map(|_| Constraint::Ratio(1, cols as u32))
            .collect();

        let col_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_chunks[row]);

        for col in 0..items_in_row {
            let idx = row * cols + col;
            let is_selected = idx == dashboard.selected;
            draw_tile(frame, dashboard, idx, is_selected, col_chunks[col], rt);
        }
    }

    // Status bar
    let metric_label = dashboard.active_metric.to_string();
    let status = Line::from(vec![
        Span::styled(
            format!(" ↑↓←→ Navigate  Enter Focus  Tab/{metric_label}  1 CPU  2 MEM  3 NET  q Quit"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(status), status_area);
}

fn draw_tile(
    frame: &mut Frame,
    dashboard: &Dashboard,
    idx: usize,
    is_selected: bool,
    area: Rect,
    rt: &tokio::runtime::Handle,
) {
    let session = &dashboard.sessions[idx];

    // Block on the async lock briefly - this is fine for UI rendering
    let data = rt.block_on(session.lock());

    let (state_icon, state_color) = match &data.state {
        SessionState::Idle => ("◌", Color::DarkGray),
        SessionState::Connecting => ("◌", Color::Yellow),
        SessionState::NeedPassword => ("⚷", Color::Yellow),
        SessionState::Authenticating => ("◌", Color::Yellow),
        SessionState::Connected => ("●", Color::Green),
        SessionState::Disconnected(_) => ("●", Color::Red),
    };

    let border_color = if is_selected {
        Color::Cyan
    } else {
        match &data.state {
            SessionState::Connected => Color::Green,
            SessionState::Disconnected(_) => Color::Red,
            _ => Color::DarkGray,
        }
    };

    let border_style = if is_selected {
        Style::default().fg(border_color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(border_color)
    };

    let title = format!(
        " {state_icon} {} ",
        data.host.name,
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            title,
            Style::default().fg(state_color).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Content inside the tile
    let content_lines = match &data.state {
        SessionState::Connected => {
            let metric = dashboard.active_metric;
            let series = data.metrics.series(metric);
            let latest = data.metrics.latest(metric);

            let metric_color = match metric {
                MetricType::Cpu => Color::Cyan,
                MetricType::Memory => Color::Magenta,
                MetricType::Network => Color::Yellow,
            };

            // Header: metric name + current value
            let value_str = match latest {
                Some(v) => {
                    if matches!(metric, MetricType::Network) {
                        format!("{:.1} KB/s", v)
                    } else {
                        format!("{:.0}%", v)
                    }
                }
                None => "--".to_string(),
            };

            let header = Line::from(vec![
                Span::styled(
                    format!("{metric} "),
                    Style::default().fg(metric_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    value_str,
                    Style::default().fg(Color::White),
                ),
            ]);
            let header_paragraph = Paragraph::new(header);

            // Split inner area: 1 line for header, rest for sparkline
            let inner_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(inner);

            frame.render_widget(header_paragraph, inner_chunks[0]);

            // Convert VecDeque<f64> to Vec<u64> for Sparkline
            let spark_data: Vec<u64> = series.iter().map(|v| *v as u64).collect();
            let sparkline = Sparkline::default()
                .data(&spark_data)
                .style(Style::default().fg(metric_color));
            frame.render_widget(sparkline, inner_chunks[1]);
            return; // We've rendered the tile contents manually
        }
        SessionState::NeedPassword => {
            vec![
                Line::from(Span::styled(
                    data.host.display_connection(),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "⚷ Password required",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    "Press Enter to focus & type password",
                    Style::default().fg(Color::DarkGray),
                )),
            ]
        }
        SessionState::Disconnected(msg) => {
            vec![
                Line::from(Span::styled(
                    data.host.display_connection(),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("✗ {msg}"),
                    Style::default().fg(Color::Red),
                )),
            ]
        }
        _ => {
            vec![
                Line::from(Span::styled(
                    data.host.display_connection(),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    data.state.label(),
                    Style::default().fg(Color::Yellow),
                )),
            ]
        }
    };

    let paragraph = Paragraph::new(content_lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn draw_focused(
    frame: &mut Frame,
    dashboard: &Dashboard,
    idx: usize,
    rt: &tokio::runtime::Handle,
) {
    let area = frame.area();
    let session = &dashboard.sessions[idx];
    let data = rt.block_on(session.lock());

    let (state_icon, state_color) = match &data.state {
        SessionState::Connected => ("●", Color::Green),
        SessionState::NeedPassword => ("⚷", Color::Yellow),
        SessionState::Disconnected(_) => ("●", Color::Red),
        _ => ("◌", Color::Yellow),
    };

    // --- Non-connected states: full-area rendering (unchanged) ---
    if !data.state.is_connected() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        let title = format!(
            " {state_icon} {} — {} ",
            data.host.name,
            data.host.display_connection(),
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(state_color))
            .title(Span::styled(
                title,
                Style::default().fg(state_color).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(chunks[0]);
        frame.render_widget(block, chunks[0]);

        match &data.state {
            SessionState::NeedPassword => {
                let password_display = if dashboard.entering_password {
                    format!("Password: {}", "*".repeat(dashboard.password_input.len()))
                } else {
                    "Press any key to enter password...".to_string()
                };
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Authentication required",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        password_display,
                        Style::default().fg(Color::White),
                    )),
                ];
                frame.render_widget(Paragraph::new(lines), inner);
            }
            SessionState::Disconnected(msg) => {
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("✗ Disconnected: {msg}"),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )),
                ];
                frame.render_widget(Paragraph::new(lines), inner);
            }
            _ => {
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        data.state.label(),
                        Style::default().fg(Color::Yellow),
                    )),
                ];
                frame.render_widget(Paragraph::new(lines), inner);
            }
        }

        let status = Line::from(Span::styled(
            " Esc Back",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(status), chunks[1]);
        return;
    }

    // === Connected state: multi-panel layout ===

    // Outer vertical: top content (70%) | bottom terminal+status (30%)
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    // --- Top area: sidebar (20%) | right panels (80%) ---
    let top_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(outer[0]);

    let sidebar_area = top_cols[0];
    let right_area = top_cols[1];

    // Right area: info row (4 lines) | main panel (remaining)
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(right_area);

    let info_row = right_rows[0];
    let main_panel_area = right_rows[1];

    // Info row: disk (50%) | system (50%)
    let info_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(info_row);

    let disk_area = info_cols[0];
    let sysinfo_area = info_cols[1];

    // Bottom: terminal | status bar
    let bottom_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(outer[1]);

    let terminal_area = bottom_rows[0];
    let status_area = bottom_rows[1];

    // Determine border color for each panel based on focus state
    let panel_border = |panel: FocusPanel| -> Color {
        if dashboard.focus_panel == panel {
            match dashboard.focus_state {
                FocusState::PanelSelect => Color::Cyan,
                FocusState::PanelFocused => Color::Green,
            }
        } else {
            Color::DarkGray
        }
    };
    let panel_title_bold = |panel: FocusPanel| -> bool {
        dashboard.focus_panel == panel
    };

    // ─── File Browser Sidebar ───
    draw_sidebar(frame, &data.file_browser, sidebar_area, panel_border(FocusPanel::Sidebar), panel_title_bold(FocusPanel::Sidebar));

    // ─── Disk Info Panel ───
    draw_disk_panel(frame, &data.disks, data.disk_loading, disk_area, panel_border(FocusPanel::Disk), panel_title_bold(FocusPanel::Disk));

    // ─── System Info Panel ───
    draw_sysinfo_panel(frame, &data.system_info, &data.host.name, sysinfo_area, panel_border(FocusPanel::SysInfo), panel_title_bold(FocusPanel::SysInfo));

    // ─── Main Panel (process viewer or file content) ───
    draw_main_panel(frame, &data, main_panel_area, panel_border(FocusPanel::Main), panel_title_bold(FocusPanel::Main));

    // ─── Terminal Pane ───
    {
        let term_title = format!(" {state_icon} Terminal ");
        let term_border_color = panel_border(FocusPanel::Terminal);
        let term_title_style = if panel_title_bold(FocusPanel::Terminal) {
            Style::default().fg(state_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(state_color)
        };
        let term_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(term_border_color))
            .title(Span::styled(term_title, term_title_style));
        let term_inner = term_block.inner(terminal_area);
        frame.render_widget(term_block, terminal_area);

        let screen = data.screen.screen();
        let rows = term_inner.height as usize;
        let total_rows = screen.size().0 as usize;
        let start = if total_rows > rows { total_rows - rows } else { 0 };

        let lines: Vec<Line> = (start..total_rows)
            .map(|r| {
                let r16 = r as u16;
                let content = screen.contents_between(r16, 0, r16 + 1, 0);
                Line::from(Span::styled(content, Style::default().fg(Color::White)))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), term_inner);
    }

    // ─── Status Bar ───
    let panel_label = match dashboard.focus_panel {
        FocusPanel::Sidebar => "Sidebar",
        FocusPanel::Disk => "Disk",
        FocusPanel::SysInfo => "SysInfo",
        FocusPanel::Main => "Main",
        FocusPanel::Terminal => "Terminal",
    };
    let status = match dashboard.focus_state {
        FocusState::PanelSelect => Line::from(vec![
            Span::styled(" ↑↓←→ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Select Panel  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Focus In  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Back to Grid  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[{panel_label}]"),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        FocusState::PanelFocused => {
            let extra_hint = match dashboard.focus_panel {
                FocusPanel::Sidebar => "  ↑↓ Navigate  Enter Open  ←/⌫ Go Up",
                FocusPanel::Terminal => "  (Input forwarded to SSH)",
                _ => "  (View only)",
            };
            Line::from(vec![
                Span::styled(" Esc ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled("Back to Panel Select  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("[{panel_label}]"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(extra_hint, Style::default().fg(Color::DarkGray)),
            ])
        }
    };
    frame.render_widget(Paragraph::new(status), status_area);
}

// ─── Helper: File Browser Sidebar ───
fn draw_sidebar(
    frame: &mut Frame,
    fb: &crate::file_browser::FileBrowserState,
    area: Rect,
    border_color: Color,
    is_active: bool,
) {
    // border_color is now passed in from the caller

    // Shorten path for title
    let path_display = if fb.current_path.len() > (area.width as usize).saturating_sub(4) {
        let parts: Vec<&str> = fb.current_path.rsplitn(2, '/').collect();
        if parts.len() > 1 {
            format!("…/{}", parts[0])
        } else {
            fb.current_path.clone()
        }
    } else {
        fb.current_path.clone()
    };

    let sidebar_title_style = if is_active {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" {path_display} "),
            sidebar_title_style,
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if fb.loading {
        let lines = vec![Line::from(Span::styled(
            "⏳ Loading...",
            Style::default().fg(Color::Yellow),
        ))];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    if let Some(err) = &fb.error {
        let lines = vec![Line::from(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        ))];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        return;
    }

    if fb.entries.is_empty() {
        let lines = vec![Line::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        ))];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // Compute visible window around selected item
    let visible_height = inner.height as usize;
    let total = fb.entries.len();
    let selected = fb.selected;

    let scroll_offset = if total <= visible_height {
        0
    } else if selected < visible_height / 2 {
        0
    } else if selected + visible_height / 2 >= total {
        total.saturating_sub(visible_height)
    } else {
        selected.saturating_sub(visible_height / 2)
    };

    let lines: Vec<Line> = fb
        .entries
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
        .map(|(i, entry)| {
            let icon = if entry.is_dir { "📁 " } else { "📄 " };
            let name = &entry.name;
            let style = if i == selected && is_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if i == selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().fg(Color::Blue)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(format!("{icon}{name}"), style))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

// ─── Helper: Disk Info Panel ───
fn draw_disk_panel(
    frame: &mut Frame,
    disks: &Option<Vec<crate::disk_info::DiskEntry>>,
    disk_loading: bool,
    area: Rect,
    border_color: Color,
    title_bold: bool,
) {
    let title_style = if title_bold {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(" Disk ", title_style));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if disk_loading && disks.is_none() {
        let lines = vec![Line::from(Span::styled(
            "⏳ Loading...",
            Style::default().fg(Color::Yellow),
        ))];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let Some(disk_entries) = disks else {
        let lines = vec![Line::from(Span::styled(
            "No data",
            Style::default().fg(Color::DarkGray),
        ))];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    };

    let lines: Vec<Line> = disk_entries
        .iter()
        .take(inner.height as usize)
        .map(|d| {
            let bar_color = if d.percent < 70 {
                Color::Green
            } else if d.percent < 90 {
                Color::Yellow
            } else {
                Color::Red
            };

            // Build a small bar: width ~8 chars
            let bar_width = 8usize;
            let filled = ((d.percent as usize) * bar_width / 100).min(bar_width);
            let empty = bar_width - filled;
            let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));

            Line::from(vec![
                Span::styled(
                    format!("{:<6}", d.mount),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{}/{} ", d.used, d.size),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(bar, Style::default().fg(bar_color)),
                Span::styled(
                    format!(" {}%", d.percent),
                    Style::default().fg(bar_color),
                ),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

// ─── Helper: System Info Panel ───
fn draw_sysinfo_panel(
    frame: &mut Frame,
    sys_info: &Option<crate::system_info::SystemInfo>,
    host_name: &str,
    area: Rect,
    border_color: Color,
    title_bold: bool,
) {
    let title_style = if title_bold {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(format!(" {host_name} "), title_style));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(info) = sys_info else {
        let lines = vec![Line::from(Span::styled(
            "Collecting...",
            Style::default().fg(Color::Yellow),
        ))];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    };

    let lines = vec![
        Line::from(Span::styled(
            info.os.clone(),
            Style::default().fg(Color::White),
        )),
        Line::from(vec![
            Span::styled(
                info.kernel.clone(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("Up {}", info.uptime),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                info.cpu_info.clone(),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("RAM {}", info.ram_total),
                Style::default().fg(Color::Magenta),
            ),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}

// ─── Helper: Main Panel (process viewer or file content) ───
fn draw_main_panel(
    frame: &mut Frame,
    data: &crate::session::SessionData,
    area: Rect,
    border_color: Color,
    title_bold: bool,
) {
    let title_style = if title_bold {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    // If viewing a file, show file content
    if let Some(filename) = &data.file_browser.viewing_file {
        let file_title_style = if title_bold {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                format!(" 📄 {filename} "),
                file_title_style,
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if let Some(content) = &data.file_browser.file_content {
            let lines: Vec<Line> = content
                .lines()
                .enumerate()
                .take(inner.height as usize)
                .map(|(i, line)| {
                    Line::from(vec![
                        Span::styled(
                            format!("{:>4} │ ", i + 1),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(line.to_string(), Style::default().fg(Color::White)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        return;
    }

    // Default: process viewer with metrics bars
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(" Main ", title_style));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split: metrics bars (2 lines) | process table (remaining)
    let inner_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    // Metrics bars
    let cpu_val = data.metrics.latest(MetricType::Cpu).unwrap_or(0.0);
    let mem_val = data.metrics.latest(MetricType::Memory).unwrap_or(0.0);

    let bar_width = 12usize;
    let cpu_filled = ((cpu_val as usize) * bar_width / 100).min(bar_width);
    let cpu_empty = bar_width - cpu_filled;
    let mem_filled = ((mem_val as usize) * bar_width / 100).min(bar_width);
    let mem_empty = bar_width - mem_filled;

    let metrics_lines = vec![
        Line::from(vec![
            Span::styled(" CPU ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("[", Style::default().fg(Color::DarkGray)),
            Span::styled("█".repeat(cpu_filled), Style::default().fg(Color::Cyan)),
            Span::styled("░".repeat(cpu_empty), Style::default().fg(Color::DarkGray)),
            Span::styled("] ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{cpu_val:.0}%"), Style::default().fg(Color::Cyan)),
            Span::styled("    ", Style::default()),
            Span::styled("MEM ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::styled("[", Style::default().fg(Color::DarkGray)),
            Span::styled("█".repeat(mem_filled), Style::default().fg(Color::Magenta)),
            Span::styled("░".repeat(mem_empty), Style::default().fg(Color::DarkGray)),
            Span::styled("] ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{mem_val:.0}%"), Style::default().fg(Color::Magenta)),
        ]),
        Line::from(""),
    ];
    frame.render_widget(Paragraph::new(metrics_lines), inner_rows[0]);

    // Process table
    let Some(processes) = &data.processes else {
        let lines = vec![Line::from(Span::styled(
            "⏳ Collecting processes...",
            Style::default().fg(Color::Yellow),
        ))];
        frame.render_widget(Paragraph::new(lines), inner_rows[1]);
        return;
    };

    let mut proc_lines: Vec<Line> = Vec::new();

    // Header
    proc_lines.push(Line::from(Span::styled(
        format!(
            " {:<7} {:<10} {:>5} {:>5}  {}",
            "PID", "USER", "CPU%", "MEM%", "COMMAND"
        ),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )));

    // Rows
    let max_rows = (inner_rows[1].height as usize).saturating_sub(1); // 1 for header
    for proc in processes.iter().take(max_rows) {
        let cmd_maxlen = (inner_rows[1].width as usize).saturating_sub(32);
        let cmd_display = if proc.command.len() > cmd_maxlen {
            format!("{}…", &proc.command[..cmd_maxlen.saturating_sub(1)])
        } else {
            proc.command.clone()
        };

        proc_lines.push(Line::from(Span::styled(
            format!(
                " {:<7} {:<10} {:>5.1} {:>5.1}  {}",
                proc.pid, proc.user, proc.cpu, proc.mem, cmd_display
            ),
            Style::default().fg(Color::White),
        )));
    }

    frame.render_widget(Paragraph::new(proc_lines), inner_rows[1]);
}
