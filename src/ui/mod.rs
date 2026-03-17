pub mod dashboard;

use ratatui::Frame;

use crate::dashboard::Dashboard;

/// Draw the dashboard UI.
pub fn draw_dashboard(frame: &mut Frame, dashboard: &mut Dashboard, rt: &tokio::runtime::Handle) {
    dashboard::draw(frame, dashboard, rt);
}
