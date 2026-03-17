use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Sparkline, Wrap},
    Frame,
};

use crate::dashboard::{Dashboard, FocusPanel, FocusState};
use crate::grid_layout::MAX_HOSTS_PER_ROW;
use crate::metrics::MetricType;
use crate::session::SessionState;

/// Draw the dashboard view.
pub fn draw(frame: &mut Frame, dashboard: &Dashboard, rt: &tokio::runtime::Handle) {
    match dashboard.focused {
        Some((row, col)) => draw_focused(frame, dashboard, row, col, rt),
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

    // Count total visual rows: each DashboardRow gets a header line (1) + tile row (remaining)
    let num_rows = dashboard.rows.len();
    let has_hidden = dashboard.show_hidden && !dashboard.hidden_hosts.is_empty();

    // Each row gets: 1 line for header + equal share of remaining height for tiles
    let total_sections = num_rows + if has_hidden { 1 } else { 0 };
    if total_sections == 0 {
        // Nothing to render
        let status = Line::from(Span::styled(
            " No hosts configured. q Quit",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(status), status_area);
        return;
    }

    // Build constraints: for each row, header(1) + tiles(ratio)
    let mut constraints: Vec<Constraint> = Vec::new();
    for _ in 0..num_rows {
        constraints.push(Constraint::Length(1)); // row header
        constraints.push(Constraint::Ratio(1, total_sections as u32)); // tiles
    }
    if has_hidden {
        constraints.push(Constraint::Length(1)); // hidden header
        constraints.push(Constraint::Ratio(1, total_sections as u32)); // hidden tiles
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(grid_area);

    // Render each dashboard row
    for (row_idx, dash_row) in dashboard.rows.iter().enumerate() {
        let header_area = chunks[row_idx * 2];
        let tiles_area = chunks[row_idx * 2 + 1];

        // --- Row header ---
        let is_renaming = dashboard.rename_mode && dashboard.selected_row == row_idx;
        let header_text = if is_renaming {
            format!(" Row name: {}_ ", dashboard.rename_input)
        } else {
            let name = &dash_row.name;
            let count = dash_row.hosts.len();
            format!("── {name} ({count}) ──")
        };

        let header_style = if is_renaming {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else if dashboard.selected_row == row_idx && !dashboard.in_hidden_section {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(header_text, header_style))),
            header_area,
        );

        // --- Tiles in this row ---
        if dash_row.hosts.is_empty() {
            continue;
        }

        let cols = dash_row.hosts.len();
        let tile_width = tiles_area.width / MAX_HOSTS_PER_ROW as u16;
        let row_width = tile_width * cols as u16;
        let col_constraints: Vec<Constraint> = (0..cols)
            .map(|_| Constraint::Length(tile_width))
            .collect();

        let row_area = Rect::new(tiles_area.x, tiles_area.y, row_width, tiles_area.height);
        let col_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_area);

        for (col_idx, session) in dash_row.hosts.iter().enumerate() {
            let is_selected = !dashboard.in_hidden_section
                && dashboard.selected_row == row_idx
                && dashboard.selected_col == col_idx;

            let is_moving = dashboard.move_mode && is_selected;

            // Shake: check if this tile is the moving host and shake is active for this row
            let is_shaking = dashboard.move_mode
                && is_selected
                && dashboard.shake_frame.as_ref().is_some_and(|(_, counter)| *counter > 0);

            draw_tile(
                frame,
                session,
                dashboard,
                is_selected,
                is_moving,
                is_shaking,
                col_chunks[col_idx],
                rt,
            );
        }
    }

    // --- Hidden section ---
    if has_hidden {
        let hidden_header_idx = num_rows * 2;
        let hidden_tiles_idx = hidden_header_idx + 1;
        let hidden_header_area = chunks[hidden_header_idx];
        let hidden_tiles_area = chunks[hidden_tiles_idx];

        let hidden_count = dashboard.hidden_hosts.len();
        let arrow = if dashboard.show_hidden { "▾" } else { "▸" };
        let header_style = if dashboard.in_hidden_section {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("─── Hidden ({hidden_count}) {arrow} ───"),
                header_style,
            ))),
            hidden_header_area,
        );

        // Render hidden host tiles
        let cols = dashboard.hidden_hosts.len();
        if cols > 0 {
            let tile_width = hidden_tiles_area.width / MAX_HOSTS_PER_ROW as u16;
            let row_width = tile_width * cols as u16;
            let col_constraints: Vec<Constraint> = (0..cols)
                .map(|_| Constraint::Length(tile_width))
                .collect();

            let hidden_row_area = Rect::new(hidden_tiles_area.x, hidden_tiles_area.y, row_width, hidden_tiles_area.height);
            let col_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(col_constraints)
                .split(hidden_row_area);

            for (i, session) in dashboard.hidden_hosts.iter().enumerate() {
                let is_selected = dashboard.in_hidden_section && dashboard.hidden_selected == i;
                draw_tile(
                    frame,
                    session,
                    dashboard,
                    is_selected,
                    false,
                    false,
                    col_chunks[i],
                    rt,
                );
            }
        }
    }

    // Status bar
    let status = if dashboard.rename_mode {
        Line::from(vec![
            Span::styled(" Type name  ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Enter ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Confirm  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
        ])
    } else if dashboard.move_mode {
        Line::from(vec![
            Span::styled(" ↑↓←→ ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("Move  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Space ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Place  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Cancel", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        let metric_label = dashboard.active_metric.to_string();
        Line::from(vec![
            Span::styled(
                format!(" ↑↓←→ Navigate  Enter Focus  Space Move  r Rename  m Hide  H Hidden  Tab/{metric_label}  q Quit"),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(status), status_area);
}

fn draw_tile(
    frame: &mut Frame,
    session: &crate::session::SharedSession,
    dashboard: &Dashboard,
    is_selected: bool,
    is_moving: bool,
    is_shaking: bool,
    area: Rect,
    rt: &tokio::runtime::Handle,
) {
    let data = rt.block_on(session.lock());

    let (state_icon, state_color) = match &data.state {
        SessionState::Idle => ("◌", Color::DarkGray),
        SessionState::Connecting => ("◌", Color::Yellow),
        SessionState::NeedPassword => ("⚷", Color::Yellow),
        SessionState::Authenticating => ("◌", Color::Yellow),
        SessionState::Connected => ("●", Color::Green),
        SessionState::Disconnected(_) => ("●", Color::Red),
    };

    let border_color = if is_shaking {
        // Shake: alternate Red/Normal based on counter parity
        if let Some((_, counter)) = dashboard.shake_frame {
            if counter % 2 == 1 { Color::Red } else { Color::Yellow }
        } else {
            Color::Yellow
        }
    } else if is_moving {
        Color::Yellow
    } else if is_selected {
        Color::Cyan
    } else {
        match &data.state {
            SessionState::Connected => Color::Green,
            SessionState::Disconnected(_) => Color::Red,
            _ => Color::DarkGray,
        }
    };

    let border_style = if is_selected || is_moving {
        Style::default().fg(border_color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(border_color)
    };

    let title_icon = if is_moving { "✦" } else { state_icon };
    let title = format!(" {title_icon} {} ", data.host.name);

    let title_color = if is_moving { Color::Yellow } else { state_color };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            title,
            Style::default().fg(title_color).add_modifier(Modifier::BOLD),
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
                Span::styled(value_str, Style::default().fg(Color::White)),
            ]);
            let header_paragraph = Paragraph::new(header);

            let inner_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(inner);

            frame.render_widget(header_paragraph, inner_chunks[0]);

            let spark_data: Vec<u64> = series.iter().map(|v| *v as u64).collect();
            let sparkline = Sparkline::default()
                .data(&spark_data)
                .style(Style::default().fg(metric_color));
            frame.render_widget(sparkline, inner_chunks[1]);
            return;
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
    row: usize,
    col: usize,
    rt: &tokio::runtime::Handle,
) {
    let area = frame.area();
    let session = match dashboard.session_at(row, col) {
        Some(s) => s,
        None => return,
    };
    let data = rt.block_on(session.lock());

    let (state_icon, state_color) = match &data.state {
        SessionState::Connected => ("●", Color::Green),
        SessionState::NeedPassword => ("⚷", Color::Yellow),
        SessionState::Disconnected(_) => ("●", Color::Red),
        _ => ("◌", Color::Yellow),
    };

    // --- Non-connected states: full-area rendering ---
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

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let top_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(outer[0]);

    let sidebar_area = top_cols[0];
    let right_area = top_cols[1];

    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(1)])
        .split(right_area);

    let info_row = right_rows[0];
    let main_panel_area = right_rows[1];

    let info_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(info_row);

    let disk_area = info_cols[0];
    let sysinfo_area = info_cols[1];

    let bottom_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(outer[1]);

    let terminal_area = bottom_rows[0];
    let status_area = bottom_rows[1];

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

    // ─── Main Panel ───
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
        let pane_rows = term_inner.height as usize;
        let total_rows = screen.size().0 as usize;

        let mut last_content_row = 0;
        for r in (0..total_rows).rev() {
            let r16 = r as u16;
            let content = screen.contents_between(r16, 0, r16 + 1, 0);
            if !content.trim().is_empty() {
                last_content_row = r;
                break;
            }
        }

        let end = (last_content_row + 1).min(total_rows);
        let start = if end > pane_rows { end - pane_rows } else { 0 };

        let lines: Vec<Line> = (start..end)
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
                FocusPanel::Sidebar => "  ↑↓ Navigate  Enter Open  ←/⌫ Up  /Search  gGoto",
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

    let header_lines: usize = if fb.goto_mode {
        1 + fb.goto_suggestions.len().min(6)
    } else if fb.search_mode {
        1
    } else {
        0
    };

    let entries_height = (inner.height as usize).saturating_sub(header_lines);

    let search_lower = if fb.search_mode && !fb.search_query.is_empty() {
        Some(fb.search_query.to_lowercase())
    } else {
        None
    };

    let filtered_entries: Vec<(usize, &crate::file_browser::FileEntry)> = fb
        .entries
        .iter()
        .enumerate()
        .filter(|(_i, entry)| {
            if let Some(q) = &search_lower {
                entry.name.to_lowercase().contains(q)
            } else {
                true
            }
        })
        .collect();

    if filtered_entries.is_empty() && !fb.search_mode && !fb.goto_mode {
        let lines = vec![Line::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        ))];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let mut all_lines: Vec<Line> = Vec::new();

    if fb.search_mode {
        all_lines.push(Line::from(Span::styled(
            format!("🔍 {}_", fb.search_query),
            Style::default().fg(Color::Yellow),
        )));
    } else if fb.goto_mode {
        all_lines.push(Line::from(Span::styled(
            format!("📂 {}_", fb.goto_path),
            Style::default().fg(Color::Yellow),
        )));
        for (si, suggestion) in fb.goto_suggestions.iter().take(6).enumerate() {
            let suffix = if suggestion.is_dir { "/" } else { "" };
            let style = if si == fb.goto_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if suggestion.is_dir {
                Style::default().fg(Color::Blue)
            } else {
                Style::default().fg(Color::White)
            };
            all_lines.push(Line::from(Span::styled(
                format!("  {}{}", suggestion.name, suffix),
                style,
            )));
        }
    }

    let total = filtered_entries.len();
    let selected = fb.selected;

    let scroll_offset = if total <= entries_height {
        0
    } else if selected < entries_height / 2 {
        0
    } else if selected + entries_height / 2 >= total {
        total.saturating_sub(entries_height)
    } else {
        selected.saturating_sub(entries_height / 2)
    };

    for (i, entry) in filtered_entries
        .iter()
        .skip(scroll_offset)
        .take(entries_height)
    {
        let icon = if entry.is_dir { "📁 " } else { "📄 " };
        let name = &entry.name;
        let style = if *i == selected && is_active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if *i == selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if entry.is_dir {
            Style::default().fg(Color::Blue)
        } else {
            Style::default().fg(Color::White)
        };
        all_lines.push(Line::from(Span::styled(format!("{icon}{name}"), style)));
    }

    frame.render_widget(Paragraph::new(all_lines), inner);
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(" Main ", title_style));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let inner_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

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

    let Some(processes) = &data.processes else {
        let lines = vec![Line::from(Span::styled(
            "⏳ Collecting processes...",
            Style::default().fg(Color::Yellow),
        ))];
        frame.render_widget(Paragraph::new(lines), inner_rows[1]);
        return;
    };

    let mut proc_lines: Vec<Line> = Vec::new();

    proc_lines.push(Line::from(Span::styled(
        format!(
            " {:<7} {:<10} {:>5} {:>5}  {}",
            "PID", "USER", "CPU%", "MEM%", "COMMAND"
        ),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )));

    let max_rows = (inner_rows[1].height as usize).saturating_sub(1);
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
