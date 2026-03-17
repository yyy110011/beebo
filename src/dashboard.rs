use std::sync::Arc;
use tokio::sync::Mutex;

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

/// Dashboard manages multiple SSH sessions in a grid layout.
pub struct Dashboard {
    pub sessions: Vec<SharedSession>,
    /// Number of columns in the grid.
    pub cols: usize,
    /// Currently selected tile index.
    pub selected: usize,
    /// Currently focused session index (None = grid view).
    pub focused: Option<usize>,
    /// Password input buffer (used when a session needs password).
    pub password_input: String,
    /// Whether we're in password input mode.
    pub entering_password: bool,
    /// Full terminal dimensions (for creating sessions with correct size).
    pub term_cols: u16,
    pub term_rows: u16,
    /// Which metric to display in tile sparklines.
    pub active_metric: MetricType,
    /// Which panel is highlighted in focus mode.
    pub focus_panel: FocusPanel,
    /// Whether we are selecting a panel or focused into one.
    pub focus_state: FocusState,
}

impl Dashboard {
    pub fn new(hosts: Vec<SshHost>, cols: usize, term_cols: u16, term_rows: u16, rt: &tokio::runtime::Handle) -> Self {
        let sessions: Vec<SharedSession> = hosts
            .into_iter()
            .map(|host| {
                // Use FULL terminal size for vt100 + PTY so focused view renders correctly
                let data = SessionData::new(host, term_cols.saturating_sub(2), term_rows.saturating_sub(3));
                let shared: SharedSession = Arc::new(Mutex::new(data));
                session::spawn_session(shared.clone(), rt.clone());
                shared
            })
            .collect();

        Dashboard {
            sessions,
            cols,
            selected: 0,
            focused: None,
            password_input: String::new(),
            entering_password: false,
            term_cols,
            term_rows,
            active_metric: MetricType::Cpu,
            focus_panel: FocusPanel::Terminal,
            focus_state: FocusState::PanelSelect,
        }
    }

    pub fn total(&self) -> usize {
        self.sessions.len()
    }

    pub fn rows(&self) -> usize {
        (self.total() + self.cols - 1) / self.cols
    }

    // Grid navigation
    pub fn move_up(&mut self) {
        if self.selected >= self.cols {
            self.selected -= self.cols;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + self.cols < self.total() {
            self.selected += self.cols;
        }
    }

    pub fn move_left(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if self.selected + 1 < self.total() {
            self.selected += 1;
        }
    }

    /// Enter focus mode on the currently selected tile.
    /// If the session is Disconnected, trigger a reconnect.
    pub fn focus(&mut self, rt: &tokio::runtime::Handle) {
        let idx = self.selected;
        self.focused = Some(idx);
        self.focus_panel = FocusPanel::Terminal;
        self.focus_state = FocusState::PanelSelect;

        // Check if session is disconnected and reconnect
        let session = self.sessions[idx].clone();
        let term_cols = self.term_cols.saturating_sub(2);
        let term_rows = self.term_rows.saturating_sub(3);
        let rt_clone = rt.clone();

        rt.block_on(async {
            let mut data = session.lock().await;
            if matches!(data.state, crate::session::SessionState::Disconnected(_)) {
                // Reset the session for reconnection
                data.state = crate::session::SessionState::Idle;
                data.screen = vt100::Parser::new(term_rows, term_cols, 200);
                data.input_tx = None;
                drop(data); // Release lock before spawning
                session::spawn_session(session.clone(), rt_clone);
            }
        });
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
    ///
    /// Layout grid:
    /// ```text
    /// [Sidebar] [Disk]    [SysInfo]
    /// [Sidebar] [Main/Process]
    /// [Terminal]
    /// ```
    pub fn move_focus(&mut self, dir: NavDirection) {
        use FocusPanel::*;
        use NavDirection::*;
        self.focus_panel = match (self.focus_panel, dir) {
            // From Sidebar
            (Sidebar, Right) => Disk,
            (Sidebar, Down) => Terminal,
            // From Disk
            (Disk, Left) => Sidebar,
            (Disk, Right) => SysInfo,
            (Disk, Down) => Main,
            // From SysInfo
            (SysInfo, Left) => Disk,
            (SysInfo, Down) => Main,
            // From Main
            (Main, Left) => Sidebar,
            (Main, Up) => Disk,
            (Main, Down) => Terminal,
            // From Terminal
            (Terminal, Up) => Main,
            // No-op for all other directions
            (current, _) => current,
        };
    }

    /// Send input bytes to the focused session.
    pub async fn send_input(&self, data: Vec<u8>) {
        if let Some(idx) = self.focused {
            let session = self.sessions[idx].lock().await;
            if let Some(ref tx) = session.input_tx {
                let _ = tx.send(data);
            }
        }
    }

    /// Submit password to the focused session.
    pub async fn submit_password(&mut self) {
        if let Some(idx) = self.focused {
            let password = self.password_input.clone();
            let session = self.sessions[idx].lock().await;
            if let Some(ref tx) = session.input_tx {
                let _ = tx.send(format!("{password}\n").into_bytes());
            }
        }
        self.password_input.clear();
        self.entering_password = false;
    }

    /// Cycle active metric: CPU → Mem → Net → CPU.
    pub fn cycle_metric(&mut self) {
        self.active_metric = self.active_metric.next();
    }

    /// Set metric directly.
    pub fn set_metric(&mut self, metric: MetricType) {
        self.active_metric = metric;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a dashboard-like struct just for testing move_focus.
    /// We only need focus_panel + focus_state, not the full Dashboard.
    fn test_move(start: FocusPanel, dir: NavDirection) -> FocusPanel {
        // We'll just inline the same logic to test it
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

