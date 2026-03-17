use std::time::Duration;

use russh_sftp::client::SftpSession;

/// Maximum file size we'll attempt to read (100 KB).
const MAX_FILE_SIZE: u64 = 100 * 1024;

/// Timeout for directory listing operations.
const DIR_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for file read operations.
const FILE_TIMEOUT: Duration = Duration::from_secs(3);

/// State of the SFTP file browser for one session.
pub struct FileBrowserState {
    pub current_path: String,
    pub entries: Vec<FileEntry>,
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
    /// Content of the currently viewed file (if any).
    pub file_content: Option<String>,
    /// Name of the file currently being viewed.
    pub viewing_file: Option<String>,
}

/// A single entry in a directory listing.
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Human-readable permissions string, e.g. `rwxr-xr-x`.
    pub permissions: String,
}

impl FileBrowserState {
    /// Create a new file browser starting at the given home directory.
    pub fn new(home_dir: String) -> Self {
        Self {
            current_path: home_dir,
            entries: Vec::new(),
            selected: 0,
            loading: false,
            error: None,
            file_content: None,
            viewing_file: None,
        }
    }

    /// Clear file viewing state and return to directory listing.
    pub fn close_file(&mut self) {
        self.file_content = None;
        self.viewing_file = None;
    }
}

// ---------------------------------------------------------------------------
// Navigation API — all timeout-protected
// ---------------------------------------------------------------------------

/// Refresh the directory listing for the current path.
pub async fn refresh_listing(sftp: &SftpSession, state: &mut FileBrowserState) {
    state.loading = true;
    state.error = None;

    match tokio::time::timeout(DIR_TIMEOUT, sftp.read_dir(&state.current_path)).await {
        Ok(Ok(entries)) => {
            let mut file_entries: Vec<FileEntry> = entries
                .into_iter()
                .filter(|e| {
                    let name = e.file_name();
                    name != "." && name != ".."
                })
                .map(|e| {
                    let meta = e.metadata();
                    let ft = e.file_type();
                    FileEntry {
                        name: e.file_name(),
                        is_dir: ft.is_dir(),
                        size: meta.size.unwrap_or(0),
                        permissions: permissions_string(meta.permissions.unwrap_or(0)),
                    }
                })
                .collect();

            // Sort: directories first, then alphabetically within each group
            file_entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });

            state.entries = file_entries;
            state.selected = 0;
        }
        Ok(Err(e)) => {
            state.error = Some(format!("SFTP error: {e}"));
            state.entries.clear();
        }
        Err(_) => {
            state.error = Some("⏳ Directory listing timed out".to_string());
        }
    }

    state.loading = false;
}

/// Navigate into the directory at the given index.
pub async fn enter_directory(sftp: &SftpSession, state: &mut FileBrowserState, index: usize) {
    if let Some(entry) = state.entries.get(index) {
        if !entry.is_dir {
            return;
        }
        let new_path = format!("{}/{}", state.current_path.trim_end_matches('/'), entry.name);
        state.current_path = new_path;
        refresh_listing(sftp, state).await;
    }
}

/// Navigate up to the parent directory.
pub async fn go_up(sftp: &SftpSession, state: &mut FileBrowserState) {
    if state.current_path == "/" {
        return;
    }
    // Remove trailing slash, then find last slash
    let trimmed = state.current_path.trim_end_matches('/');
    let parent = match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(pos) => trimmed[..pos].to_string(),
        None => "/".to_string(),
    };
    state.current_path = parent;
    refresh_listing(sftp, state).await;
}

/// Read the file at the given index. Caps content at `MAX_FILE_SIZE`.
pub async fn read_file(sftp: &SftpSession, state: &mut FileBrowserState, index: usize) {
    let (file_path, file_name, file_size) = match state.entries.get(index) {
        Some(entry) if !entry.is_dir => {
            let path =
                format!("{}/{}", state.current_path.trim_end_matches('/'), entry.name);
            (path, entry.name.clone(), entry.size)
        }
        _ => return,
    };

    if file_size > MAX_FILE_SIZE {
        state.error = Some(format!(
            "File too large ({:.1} KB, max {:.0} KB)",
            file_size as f64 / 1024.0,
            MAX_FILE_SIZE as f64 / 1024.0
        ));
        return;
    }

    state.loading = true;
    state.error = None;

    match tokio::time::timeout(FILE_TIMEOUT, sftp.read(file_path)).await {
        Ok(Ok(bytes)) => {
            state.file_content = Some(String::from_utf8_lossy(&bytes).to_string());
            state.viewing_file = Some(file_name);
        }
        Ok(Err(e)) => {
            state.error = Some(format!("Failed to read file: {e}"));
        }
        Err(_) => {
            state.error = Some("⏳ File read timed out".to_string());
        }
    }

    state.loading = false;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a Unix permission mode (e.g. `0o755`) to a human-readable string
/// like `rwxr-xr-x`. Only the lower 9 bits (user/group/other) are used.
pub fn permissions_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in (0..3).rev() {
        let bits = (mode >> (shift * 3)) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permissions_string_755() {
        assert_eq!(permissions_string(0o755), "rwxr-xr-x");
    }

    #[test]
    fn test_permissions_string_644() {
        assert_eq!(permissions_string(0o644), "rw-r--r--");
    }

    #[test]
    fn test_permissions_string_700() {
        assert_eq!(permissions_string(0o700), "rwx------");
    }

    #[test]
    fn test_permissions_string_000() {
        assert_eq!(permissions_string(0o000), "---------");
    }

    #[test]
    fn test_permissions_string_777() {
        assert_eq!(permissions_string(0o777), "rwxrwxrwx");
    }

    #[test]
    fn test_file_browser_state_new() {
        let state = FileBrowserState::new("/home/user".to_string());
        assert_eq!(state.current_path, "/home/user");
        assert!(state.entries.is_empty());
        assert_eq!(state.selected, 0);
        assert!(!state.loading);
        assert!(state.error.is_none());
        assert!(state.file_content.is_none());
        assert!(state.viewing_file.is_none());
    }

    #[test]
    fn test_close_file() {
        let mut state = FileBrowserState::new("/home/user".to_string());
        state.file_content = Some("hello".to_string());
        state.viewing_file = Some("test.txt".to_string());
        state.close_file();
        assert!(state.file_content.is_none());
        assert!(state.viewing_file.is_none());
    }

    #[test]
    fn test_file_entry_sorting() {
        let mut entries = vec![
            FileEntry {
                name: "zebra.txt".to_string(),
                is_dir: false,
                size: 100,
                permissions: "rw-r--r--".to_string(),
            },
            FileEntry {
                name: "alpha".to_string(),
                is_dir: true,
                size: 4096,
                permissions: "rwxr-xr-x".to_string(),
            },
            FileEntry {
                name: "beta.rs".to_string(),
                is_dir: false,
                size: 200,
                permissions: "rw-r--r--".to_string(),
            },
            FileEntry {
                name: "docs".to_string(),
                is_dir: true,
                size: 4096,
                permissions: "rwxr-xr-x".to_string(),
            },
        ];

        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "alpha");
        assert!(entries[1].is_dir);
        assert_eq!(entries[1].name, "docs");
        assert!(!entries[2].is_dir);
        assert_eq!(entries[2].name, "beta.rs");
        assert!(!entries[3].is_dir);
        assert_eq!(entries[3].name, "zebra.txt");
    }
}
