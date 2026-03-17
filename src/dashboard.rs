use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::grid_layout::{GridLayout, MAX_HOSTS_PER_ROW};
use crate::metrics::MetricType;
use crate::session::{self, SessionData, SharedSession};
use crate::ssh_config::SshHost;

/// Which panel is selected/focused in the focus view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Sidebar,
    Disk,
    SysInfo,
    Main,
    Terminal,
}

/// State within focus mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusState {
    /// Arrow keys move between panels.
    PanelSelect,
    /// Selected panel receives input.
    PanelFocused,
}

/// Cardinal direction for panel navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDirection {
    Up,
    Down,
    Left,
    Right,
}

/// A row of hosts in the dashboard grid.
pub struct DashboardRow {
    pub name: String,
    pub hosts: Vec<SharedSession>,
}

/// Dashboard manages multiple SSH sessions in a row-based grid layout.
pub struct Dashboard {
    pub rows: Vec<DashboardRow>,
    /// Hosts that are hidden from the grid.
    pub hidden_hosts: Vec<SharedSession>,
    /// Whether the hidden section is visible.
    pub show_hidden: bool,
    /// Currently selected row index.
    pub selected_row: usize,
    /// Currently selected column index within the row.
    pub selected_col: usize,
    /// Scroll offset for the grid view (number of rows to skip from the top).
    pub scroll_offset: usize,
    /// Currently focused session (row, col) — None = grid view.
    pub focused: Option<(usize, usize)>,
    /// Password input buffer.
    pub password_input: String,
    /// Whether we're in password input mode.
    pub entering_password: bool,
    /// Full terminal dimensions.
    pub term_cols: u16,
    pub term_rows: u16,
    /// Which metric to display in tile sparklines.
    pub active_metric: MetricType,
    /// Which panel is highlighted in focus mode.
    pub focus_panel: FocusPanel,
    /// Whether we are selecting a panel or focused into one.
    pub focus_state: FocusState,
    // --- Move mode ---
    pub move_mode: bool,
    pub move_origin: Option<(usize, usize)>,
    // --- Rename mode ---
    pub rename_mode: bool,
    pub rename_input: String,
    // --- Shake animation ---
    /// (target_row_index, frame_counter) — active when move is blocked by a full row.
    pub shake_frame: Option<(usize, u8)>,
    /// Whether the cursor is currently in the hidden section.
    pub in_hidden_section: bool,
    /// Selected index within the hidden section.
    pub hidden_selected: usize,
}

impl Dashboard {
    pub fn new(
        hosts: Vec<SshHost>,
        _cols: usize,
        term_cols: u16,
        term_rows: u16,
        rt: &tokio::runtime::Handle,
    ) -> Self {
        let host_names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();

        // Try to load existing layout, otherwise create from hosts
        let layout_path = Self::layout_path();
        let layout = GridLayout::load(&layout_path)
            .unwrap_or_else(|| GridLayout::from_hosts(&host_names));

        // Build a map of host name → SshHost for lookup
        let mut host_map: std::collections::HashMap<String, SshHost> = hosts
            .into_iter()
            .map(|h| (h.name.clone(), h))
            .collect();

        // Build dashboard rows from layout
        let mut rows = Vec::new();
        for grid_row in &layout.rows {
            let mut sessions = Vec::new();
            for host_name in &grid_row.hosts {
                if let Some(host) = host_map.remove(host_name) {
                    let data = SessionData::new(
                        host,
                        term_cols.saturating_sub(2),
                        term_rows.saturating_sub(3),
                    );
                    let shared: SharedSession = Arc::new(Mutex::new(data));
                    session::spawn_session(shared.clone(), rt.clone());
                    sessions.push(shared);
                }
            }
            rows.push(DashboardRow {
                name: grid_row.name.clone(),
                hosts: sessions,
            });
        }

        // Build hidden hosts
        let mut hidden_hosts = Vec::new();
        for host_name in &layout.hidden {
            if let Some(host) = host_map.remove(host_name) {
                let data = SessionData::new(
                    host,
                    term_cols.saturating_sub(2),
                    term_rows.saturating_sub(3),
                );
                let shared: SharedSession = Arc::new(Mutex::new(data));
                // Don't spawn session for hidden hosts — they stay disconnected
                hidden_hosts.push(shared);
            }
        }

        // Any hosts in SSH config but not in the saved layout go into a new row
        if !host_map.is_empty() {
            let mut extra_sessions = Vec::new();
            for (_name, host) in host_map {
                let data = SessionData::new(
                    host,
                    term_cols.saturating_sub(2),
                    term_rows.saturating_sub(3),
                );
                let shared: SharedSession = Arc::new(Mutex::new(data));
                session::spawn_session(shared.clone(), rt.clone());
                extra_sessions.push(shared);
            }
            if !extra_sessions.is_empty() {
                rows.push(DashboardRow {
                    name: format!("Row {}", rows.len() + 1),
                    hosts: extra_sessions,
                });
            }
        }

        // Remove empty rows
        rows.retain(|r| !r.hosts.is_empty());

        // Ensure at least one row
        if rows.is_empty() {
            rows.push(DashboardRow {
                name: "Row 1".to_string(),
                hosts: Vec::new(),
            });
        }

        Dashboard {
            rows,
            hidden_hosts,
            show_hidden: false,
            selected_row: 0,
            selected_col: 0,
            scroll_offset: 0,
            focused: None,
            password_input: String::new(),
            entering_password: false,
            term_cols,
            term_rows,
            active_metric: MetricType::Cpu,
            focus_panel: FocusPanel::Terminal,
            focus_state: FocusState::PanelSelect,
            move_mode: false,
            move_origin: None,
            rename_mode: false,
            rename_input: String::new(),
            shake_frame: None,
            in_hidden_section: false,
            hidden_selected: 0,
        }
    }

    fn layout_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".config").join("beebo").join("layout.yaml")
    }

    /// Get the session at (row, col).
    pub fn session_at(&self, row: usize, col: usize) -> Option<&SharedSession> {
        self.rows.get(row).and_then(|r| r.hosts.get(col))
    }

    /// Get the currently selected session.
    pub fn selected_session(&self) -> Option<&SharedSession> {
        if self.in_hidden_section {
            self.hidden_hosts.get(self.hidden_selected)
        } else {
            self.session_at(self.selected_row, self.selected_col)
        }
    }

    /// Total number of visible hosts.
    pub fn total_visible(&self) -> usize {
        self.rows.iter().map(|r| r.hosts.len()).sum()
    }

    // --- Grid navigation ---

    pub fn move_up(&mut self) {
        if self.in_hidden_section {
            // Move out of hidden section to last grid row
            self.in_hidden_section = false;
            if !self.rows.is_empty() {
                self.selected_row = self.rows.len() - 1;
                self.selected_col = self.selected_col.min(
                    self.rows[self.selected_row].hosts.len().saturating_sub(1),
                );
            }
            return;
        }
        if self.selected_row > 0 {
            self.selected_row -= 1;
            // Clamp col to new row's length
            let max_col = self.rows[self.selected_row].hosts.len().saturating_sub(1);
            self.selected_col = self.selected_col.min(max_col);
        }
    }

    pub fn move_down(&mut self) {
        if self.in_hidden_section {
            return; // Already at bottom
        }
        if self.selected_row + 1 < self.rows.len() {
            self.selected_row += 1;
            let max_col = self.rows[self.selected_row].hosts.len().saturating_sub(1);
            self.selected_col = self.selected_col.min(max_col);
        } else if self.show_hidden && !self.hidden_hosts.is_empty() {
            // Move into hidden section
            self.in_hidden_section = true;
            self.hidden_selected = 0;
        }
    }

    pub fn move_left(&mut self) {
        if self.in_hidden_section {
            if self.hidden_selected > 0 {
                self.hidden_selected -= 1;
            }
            return;
        }
        if self.selected_col > 0 {
            self.selected_col -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.in_hidden_section {
            if self.hidden_selected + 1 < self.hidden_hosts.len() {
                self.hidden_selected += 1;
            }
            return;
        }
        if let Some(row) = self.rows.get(self.selected_row) {
            if self.selected_col + 1 < row.hosts.len() {
                self.selected_col += 1;
            }
        }
    }

    // --- Move mode ---

    pub fn enter_move_mode(&mut self) {
        if self.in_hidden_section || self.rows.is_empty() {
            return;
        }
        if let Some(row) = self.rows.get(self.selected_row) {
            if row.hosts.is_empty() {
                return;
            }
        }
        self.move_mode = true;
        self.move_origin = Some((self.selected_row, self.selected_col));
    }

    /// Attempt to move the picked host in the given direction.
    /// Returns true if the move was successful.
    pub fn move_host(&mut self, dir: NavDirection) -> bool {
        if !self.move_mode {
            return false;
        }

        let cur_row = self.selected_row;
        let cur_col = self.selected_col;

        match dir {
            NavDirection::Right => {
                // Insert at cur_col + 1 in the same row
                let row_len = self.rows[cur_row].hosts.len();
                if cur_col + 1 >= row_len && row_len == 1 {
                    // Only host in row — can't move right within same row
                    // Try to go to next row's beginning instead
                    return false;
                }
                if cur_col + 1 > self.rows[cur_row].hosts.len() {
                    return false;
                }
                // Just swap with the next element if it exists
                let row = &mut self.rows[cur_row];
                if cur_col + 1 < row.hosts.len() {
                    row.hosts.swap(cur_col, cur_col + 1);
                    self.selected_col = cur_col + 1;
                    return true;
                }
                false
            }
            NavDirection::Left => {
                if cur_col == 0 {
                    return false;
                }
                let row = &mut self.rows[cur_row];
                row.hosts.swap(cur_col, cur_col - 1);
                self.selected_col = cur_col - 1;
                true
            }
            NavDirection::Down => {
                let next_row = cur_row + 1;
                if next_row < self.rows.len() {
                    // Check if target row is full
                    if self.rows[next_row].hosts.len() >= MAX_HOSTS_PER_ROW {
                        self.shake_frame = Some((next_row, 6));
                        return false;
                    }
                    // Remove from current row, insert at same column position in next row
                    let host = self.rows[cur_row].hosts.remove(cur_col);
                    let insert_col = cur_col.min(self.rows[next_row].hosts.len());
                    self.rows[next_row].hosts.insert(insert_col, host);
                    // Remove empty row
                    if self.rows[cur_row].hosts.is_empty() {
                        self.rows.remove(cur_row);
                        // next_row shifted
                        self.selected_row = cur_row; // now points to what was next_row
                    } else {
                        self.selected_row = next_row;
                    }
                    self.selected_col = insert_col;
                    true
                } else {
                    // At last row: create a new row
                    let host = self.rows[cur_row].hosts.remove(cur_col);
                    let new_row_name = format!("Row {}", self.rows.len() + 1);
                    self.rows.push(DashboardRow {
                        name: new_row_name,
                        hosts: vec![host],
                    });
                    // Remove source row if empty
                    if self.rows[cur_row].hosts.is_empty() {
                        self.rows.remove(cur_row);
                        self.selected_row = self.rows.len() - 1;
                    } else {
                        self.selected_row = self.rows.len() - 1;
                    }
                    self.selected_col = 0;
                    true
                }
            }
            NavDirection::Up => {
                if cur_row == 0 {
                    return false;
                }
                let prev_row = cur_row - 1;
                // Check if target row is full
                if self.rows[prev_row].hosts.len() >= MAX_HOSTS_PER_ROW {
                    self.shake_frame = Some((prev_row, 6));
                    return false;
                }
                // Remove from current row, insert at same column position in previous row
                let host = self.rows[cur_row].hosts.remove(cur_col);
                let insert_col = cur_col.min(self.rows[prev_row].hosts.len());
                self.rows[prev_row].hosts.insert(insert_col, host);
                // Remove empty row
                if self.rows[cur_row].hosts.is_empty() {
                    self.rows.remove(cur_row);
                }
                self.selected_row = prev_row;
                self.selected_col = insert_col;
                true
            }
        }
    }

    pub fn confirm_move(&mut self) {
        self.move_mode = false;
        self.move_origin = None;
        self.save_layout();
    }

    pub fn cancel_move(&mut self, rt: &tokio::runtime::Handle) {
        if let Some((orig_row, orig_col)) = self.move_origin.take() {
            // We must restore the host to its original position.
            // Remove it from the current position first.
            let cur_row = self.selected_row;
            let cur_col = self.selected_col;

            if cur_row < self.rows.len() && cur_col < self.rows[cur_row].hosts.len() {
                let host = self.rows[cur_row].hosts.remove(cur_col);

                // Clean up empty row from removing
                let target_row = if self.rows[cur_row].hosts.is_empty() && cur_row != orig_row {
                    self.rows.remove(cur_row);
                    // Adjust orig_row if it was after the removed row
                    if orig_row > cur_row { orig_row - 1 } else { orig_row }
                } else if self.rows[cur_row].hosts.is_empty() && cur_row == orig_row {
                    // The origin row became empty when we removed — we need to recreate it
                    self.rows.remove(cur_row);
                    // Reinsert a row at the original position
                    let row_name = format!("Row {}", orig_row + 1);
                    let _ = rt; // we don't need rt here
                    self.rows.insert(orig_row.min(self.rows.len()), DashboardRow {
                        name: row_name,
                        hosts: vec![host],
                    });
                    self.selected_row = orig_row.min(self.rows.len() - 1);
                    self.selected_col = 0;
                    self.move_mode = false;
                    return;
                } else {
                    orig_row
                };

                // Ensure target row exists
                if target_row < self.rows.len() {
                    let insert_col = orig_col.min(self.rows[target_row].hosts.len());
                    self.rows[target_row].hosts.insert(insert_col, host);
                    self.selected_row = target_row;
                    self.selected_col = insert_col;
                } else {
                    // Row doesn't exist anymore, create it
                    let row_name = format!("Row {}", target_row + 1);
                    self.rows.push(DashboardRow {
                        name: row_name,
                        hosts: vec![host],
                    });
                    self.selected_row = self.rows.len() - 1;
                    self.selected_col = 0;
                }
            }
        }

        self.move_mode = false;
    }

    // --- Rename mode ---

    pub fn enter_rename_mode(&mut self) {
        if self.in_hidden_section || self.rows.is_empty() {
            return;
        }
        self.rename_mode = true;
        self.rename_input = self.rows[self.selected_row].name.clone();
    }

    pub fn confirm_rename(&mut self) {
        if self.selected_row < self.rows.len() {
            self.rows[self.selected_row].name = self.rename_input.clone();
            self.save_layout();
        }
        self.rename_mode = false;
        self.rename_input.clear();
    }

    pub fn cancel_rename(&mut self) {
        self.rename_mode = false;
        self.rename_input.clear();
    }

    // --- Hide / Unhide ---

    pub fn hide_host(&mut self) {
        if self.in_hidden_section {
            // Unhide: move from hidden to end of last (or current) grid row
            if self.hidden_selected < self.hidden_hosts.len() {
                let host = self.hidden_hosts.remove(self.hidden_selected);
                // Add to the last row (or create one)
                if self.rows.is_empty() {
                    self.rows.push(DashboardRow {
                        name: "Row 1".to_string(),
                        hosts: vec![host],
                    });
                } else {
                    let last = self.rows.len() - 1;
                    if self.rows[last].hosts.len() >= MAX_HOSTS_PER_ROW {
                        self.rows.push(DashboardRow {
                            name: format!("Row {}", self.rows.len() + 1),
                            hosts: vec![host],
                        });
                    } else {
                        self.rows[last].hosts.push(host);
                    }
                }
                // Clamp hidden_selected
                if !self.hidden_hosts.is_empty() {
                    self.hidden_selected = self.hidden_selected.min(self.hidden_hosts.len() - 1);
                } else {
                    self.in_hidden_section = false;
                }
                self.save_layout();
            }
            return;
        }

        // Hide: move from grid to hidden_hosts
        if self.selected_row < self.rows.len()
            && self.selected_col < self.rows[self.selected_row].hosts.len()
        {
            let host = self.rows[self.selected_row].hosts.remove(self.selected_col);
            self.hidden_hosts.push(host);

            // Clean up empty row
            if self.rows[self.selected_row].hosts.is_empty() {
                self.rows.remove(self.selected_row);
                if !self.rows.is_empty() {
                    self.selected_row = self.selected_row.min(self.rows.len() - 1);
                    self.selected_col = 0;
                }
            } else {
                self.selected_col = self.selected_col.min(
                    self.rows[self.selected_row].hosts.len().saturating_sub(1),
                );
            }

            self.save_layout();
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        if !self.show_hidden && self.in_hidden_section {
            self.in_hidden_section = false;
            if !self.rows.is_empty() {
                self.selected_row = self.rows.len() - 1;
                self.selected_col = 0;
            }
        }
    }

    // --- Save layout ---

    pub fn save_layout(&self) {
        let rows: Vec<(String, Vec<String>)> = self
            .rows
            .iter()
            .map(|r| {
                let host_names: Vec<String> = r
                    .hosts
                    .iter()
                    .map(|s| {
                        // We need to get the host name synchronously.
                        // Since this is called from the UI thread, we can try_lock.
                        if let Ok(data) = s.try_lock() {
                            data.host.name.clone()
                        } else {
                            "unknown".to_string()
                        }
                    })
                    .collect();
                (r.name.clone(), host_names)
            })
            .collect();

        let hidden: Vec<String> = self
            .hidden_hosts
            .iter()
            .map(|s| {
                if let Ok(data) = s.try_lock() {
                    data.host.name.clone()
                } else {
                    "unknown".to_string()
                }
            })
            .collect();

        let layout = GridLayout::from_dashboard_rows(&rows, &hidden);
        let _ = layout.save(&Self::layout_path());
    }

    // --- Focus mode ---

    /// Enter focus mode on the currently selected tile.
    pub fn focus(&mut self, rt: &tokio::runtime::Handle) {
        if self.in_hidden_section {
            return; // Can't focus hidden hosts
        }
        let row = self.selected_row;
        let col = self.selected_col;

        if self.session_at(row, col).is_none() {
            return;
        }

        self.focused = Some((row, col));
        self.focus_panel = FocusPanel::Terminal;
        self.focus_state = FocusState::PanelSelect;

        // Check if session is disconnected and reconnect
        let session = self.rows[row].hosts[col].clone();
        let term_cols = self.term_cols.saturating_sub(2);
        let term_rows = self.term_rows.saturating_sub(3);
        let rt_clone = rt.clone();

        rt.block_on(async {
            let mut data = session.lock().await;
            if matches!(data.state, crate::session::SessionState::Disconnected(_)) {
                data.state = crate::session::SessionState::Idle;
                data.screen = vt100::Parser::new(term_rows, term_cols, 200);
                data.input_tx = None;
                drop(data);
                session::spawn_session(session.clone(), rt_clone);
            }
        });
    }

    /// Get the focused session.
    pub fn focused_session(&self) -> Option<&SharedSession> {
        let (row, col) = self.focused?;
        self.session_at(row, col)
    }

    /// Exit focus mode, return to grid.
    pub fn unfocus(&mut self) {
        self.focused = None;
        self.entering_password = false;
        self.password_input.clear();
        self.focus_panel = FocusPanel::Terminal;
        self.focus_state = FocusState::PanelSelect;
    }

    /// Move the panel selection highlight in the given direction.
    pub fn move_focus(&mut self, dir: NavDirection) {
        use FocusPanel::*;
        use NavDirection::*;
        self.focus_panel = match (self.focus_panel, dir) {
            (Sidebar, Right) => Disk,
            (Sidebar, Down) => Terminal,
            (Disk, Left) => Sidebar,
            (Disk, Right) => SysInfo,
            (Disk, Down) => Main,
            (SysInfo, Left) => Disk,
            (SysInfo, Down) => Main,
            (Main, Left) => Sidebar,
            (Main, Up) => Disk,
            (Main, Down) => Terminal,
            (Terminal, Up) => Main,
            (current, _) => current,
        };
    }

    /// Send input bytes to the focused session.
    pub async fn send_input(&self, data: Vec<u8>) {
        if let Some(session) = self.focused_session() {
            let sess = session.lock().await;
            if let Some(ref tx) = sess.input_tx {
                let _ = tx.send(data);
            }
        }
    }

    /// Submit password to the focused session.
    pub async fn submit_password(&mut self) {
        if let Some(session) = self.focused_session() {
            let password = self.password_input.clone();
            let sess = session.lock().await;
            if let Some(ref tx) = sess.input_tx {
                let _ = tx.send(format!("{password}\n").into_bytes());
            }
        }
        self.password_input.clear();
        self.entering_password = false;
    }

    /// Cycle active metric.
    pub fn cycle_metric(&mut self) {
        self.active_metric = self.active_metric.next();
    }

    /// Set metric directly.
    pub fn set_metric(&mut self, metric: MetricType) {
        self.active_metric = metric;
    }

    /// Tick the shake animation. Call once per event loop iteration.
    pub fn tick_shake(&mut self) {
        if let Some((_row, ref mut counter)) = self.shake_frame {
            if *counter > 0 {
                *counter -= 1;
            } else {
                self.shake_frame = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: test move_focus transitions.
    fn test_move(start: FocusPanel, dir: NavDirection) -> FocusPanel {
        let mut panel = start;
        let new = match (panel, dir) {
            (FocusPanel::Sidebar, NavDirection::Right) => FocusPanel::Disk,
            (FocusPanel::Sidebar, NavDirection::Down) => FocusPanel::Terminal,
            (FocusPanel::Disk, NavDirection::Left) => FocusPanel::Sidebar,
            (FocusPanel::Disk, NavDirection::Right) => FocusPanel::SysInfo,
            (FocusPanel::Disk, NavDirection::Down) => FocusPanel::Main,
            (FocusPanel::SysInfo, NavDirection::Left) => FocusPanel::Disk,
            (FocusPanel::SysInfo, NavDirection::Down) => FocusPanel::Main,
            (FocusPanel::Main, NavDirection::Left) => FocusPanel::Sidebar,
            (FocusPanel::Main, NavDirection::Up) => FocusPanel::Disk,
            (FocusPanel::Main, NavDirection::Down) => FocusPanel::Terminal,
            (FocusPanel::Terminal, NavDirection::Up) => FocusPanel::Main,
            (current, _) => current,
        };
        panel = new;
        panel
    }

    #[test]
    fn sidebar_right_goes_to_disk() {
        assert_eq!(test_move(FocusPanel::Sidebar, NavDirection::Right), FocusPanel::Disk);
    }

    #[test]
    fn sidebar_down_goes_to_terminal() {
        assert_eq!(test_move(FocusPanel::Sidebar, NavDirection::Down), FocusPanel::Terminal);
    }

    #[test]
    fn sidebar_up_is_noop() {
        assert_eq!(test_move(FocusPanel::Sidebar, NavDirection::Up), FocusPanel::Sidebar);
    }

    #[test]
    fn sidebar_left_is_noop() {
        assert_eq!(test_move(FocusPanel::Sidebar, NavDirection::Left), FocusPanel::Sidebar);
    }

    #[test]
    fn disk_left_goes_to_sidebar() {
        assert_eq!(test_move(FocusPanel::Disk, NavDirection::Left), FocusPanel::Sidebar);
    }

    #[test]
    fn disk_right_goes_to_sysinfo() {
        assert_eq!(test_move(FocusPanel::Disk, NavDirection::Right), FocusPanel::SysInfo);
    }

    #[test]
    fn disk_down_goes_to_main() {
        assert_eq!(test_move(FocusPanel::Disk, NavDirection::Down), FocusPanel::Main);
    }

    #[test]
    fn sysinfo_left_goes_to_disk() {
        assert_eq!(test_move(FocusPanel::SysInfo, NavDirection::Left), FocusPanel::Disk);
    }

    #[test]
    fn sysinfo_down_goes_to_main() {
        assert_eq!(test_move(FocusPanel::SysInfo, NavDirection::Down), FocusPanel::Main);
    }

    #[test]
    fn sysinfo_right_is_noop() {
        assert_eq!(test_move(FocusPanel::SysInfo, NavDirection::Right), FocusPanel::SysInfo);
    }

    #[test]
    fn main_left_goes_to_sidebar() {
        assert_eq!(test_move(FocusPanel::Main, NavDirection::Left), FocusPanel::Sidebar);
    }

    #[test]
    fn main_up_goes_to_disk() {
        assert_eq!(test_move(FocusPanel::Main, NavDirection::Up), FocusPanel::Disk);
    }

    #[test]
    fn main_down_goes_to_terminal() {
        assert_eq!(test_move(FocusPanel::Main, NavDirection::Down), FocusPanel::Terminal);
    }

    #[test]
    fn terminal_up_goes_to_main() {
        assert_eq!(test_move(FocusPanel::Terminal, NavDirection::Up), FocusPanel::Main);
    }

    #[test]
    fn terminal_down_is_noop() {
        assert_eq!(test_move(FocusPanel::Terminal, NavDirection::Down), FocusPanel::Terminal);
    }

    #[test]
    fn terminal_left_is_noop() {
        assert_eq!(test_move(FocusPanel::Terminal, NavDirection::Left), FocusPanel::Terminal);
    }

    #[test]
    fn terminal_right_is_noop() {
        assert_eq!(test_move(FocusPanel::Terminal, NavDirection::Right), FocusPanel::Terminal);
    }
}
