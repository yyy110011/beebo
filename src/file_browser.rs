use std::time::Duration;

use russh_sftp::client::SftpSession;

/// Maximum file size we'll attempt to read (100 KB).
const MAX_FILE_SIZE: u64 = 100 * 1024;

/// Timeout for directory listing operations.
const DIR_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for goto-mode suggestion fetches.
const GOTO_TIMEOUT: Duration = Duration::from_secs(2);

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

    // ── Search mode (`/`) ──
    pub search_query: String,
    pub search_mode: bool,

    // ── Goto mode (`g`) ──
    pub goto_path: String,
    pub goto_mode: bool,
    pub goto_suggestions: Vec<FileEntry>,
    pub goto_selected: usize,
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
            search_query: String::new(),
            search_mode: false,
            goto_path: String::new(),
            goto_mode: false,
            goto_suggestions: Vec::new(),
            goto_selected: 0,
        }
    }

    /// Clear file viewing state and return to directory listing.
    pub fn close_file(&mut self) {
        self.file_content = None;
        self.viewing_file = None;
    }

    /// Enter search mode.
    pub fn enter_search(&mut self) {
        self.search_mode = true;
        self.search_query.clear();
    }

    /// Exit search mode.
    pub fn exit_search(&mut self) {
        self.search_mode = false;
        self.search_query.clear();
    }

    /// Enter goto mode.
    pub fn enter_goto(&mut self) {
        self.goto_mode = true;
        self.goto_path.clear();
        self.goto_suggestions.clear();
        self.goto_selected = 0;
    }

    /// Exit goto mode.
    pub fn exit_goto(&mut self) {
        self.goto_mode = false;
        self.goto_path.clear();
        self.goto_suggestions.clear();
        self.goto_selected = 0;
    }

    /// Autocomplete the currently selected goto suggestion.
    pub fn autocomplete_selected(&mut self) {
        if let Some(entry) = self.goto_suggestions.get(self.goto_selected) {
            // Split path into dir part; replace the partial prefix with the full name
            let (dir, _prefix) = split_goto_input(&self.goto_path, &self.current_path);
            let mut completed = if dir.ends_with('/') {
                format!("{}{}", dir, entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };
            if entry.is_dir {
                completed.push('/');
            }
            self.goto_path = completed;
            self.goto_suggestions.clear();
            self.goto_selected = 0;
        }
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
// Goto-mode suggestions
// ---------------------------------------------------------------------------

/// Split a goto input into (directory_to_list, name_prefix).
///
/// - `/var/lo`  → (`/var/`, `lo`)
/// - `/etc/`    → (`/etc/`, ``)
/// - `lo`       → (`<current_path>`, `lo`)
pub fn split_goto_input<'a>(input: &'a str, current_path: &'a str) -> (String, String) {
    if input.is_empty() {
        return (current_path.to_string(), String::new());
    }
    if input.contains('/') {
        if let Some(pos) = input.rfind('/') {
            let dir = if pos == 0 { "/".to_string() } else { input[..pos].to_string() };
            let prefix = input[pos + 1..].to_string();
            (dir, prefix)
        } else {
            (current_path.to_string(), input.to_string())
        }
    } else {
        (current_path.to_string(), input.to_string())
    }
}

/// Fetch goto suggestions from SFTP based on the current goto input.
pub async fn fetch_goto_suggestions(
    sftp: &SftpSession,
    state: &mut FileBrowserState,
) {
    let (dir, prefix) = split_goto_input(&state.goto_path, &state.current_path);

    match tokio::time::timeout(GOTO_TIMEOUT, sftp.read_dir(&dir)).await {
        Ok(Ok(entries)) => {
            let prefix_lower = prefix.to_lowercase();
            let mut suggestions: Vec<FileEntry> = entries
                .into_iter()
                .filter(|e| {
                    let name = e.file_name();
                    name != "." && name != ".."
                })
                .filter(|e| {
                    prefix_lower.is_empty() || e.file_name().to_lowercase().starts_with(&prefix_lower)
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

            suggestions.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });

            state.goto_suggestions = suggestions;
            state.goto_selected = 0;
        }
        Ok(Err(_)) => {
            state.goto_suggestions.clear();
            state.goto_selected = 0;
        }
        Err(_) => {
            // Timeout — clear suggestions silently
            state.goto_suggestions.clear();
            state.goto_selected = 0;
        }
    }
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
        assert!(!state.search_mode);
        assert!(state.search_query.is_empty());
        assert!(!state.goto_mode);
        assert!(state.goto_path.is_empty());
        assert!(state.goto_suggestions.is_empty());
        assert_eq!(state.goto_selected, 0);
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
    fn test_split_goto_input_absolute() {
        let (dir, prefix) = split_goto_input("/var/lo", "/home/user");
        assert_eq!(dir, "/var");
        assert_eq!(prefix, "lo");
    }

    #[test]
    fn test_split_goto_input_trailing_slash() {
        let (dir, prefix) = split_goto_input("/etc/", "/home/user");
        assert_eq!(dir, "/etc");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_split_goto_input_relative() {
        let (dir, prefix) = split_goto_input("lo", "/home/user");
        assert_eq!(dir, "/home/user");
        assert_eq!(prefix, "lo");
    }

    #[test]
    fn test_split_goto_input_root() {
        let (dir, prefix) = split_goto_input("/", "/home/user");
        assert_eq!(dir, "/");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_split_goto_input_empty() {
        let (dir, prefix) = split_goto_input("", "/home/user");
        assert_eq!(dir, "/home/user");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_enter_exit_search() {
        let mut state = FileBrowserState::new("/home/user".to_string());
        state.enter_search();
        assert!(state.search_mode);
        state.search_query = "test".to_string();
        state.exit_search();
        assert!(!state.search_mode);
        assert!(state.search_query.is_empty());
    }

    #[test]
    fn test_enter_exit_goto() {
        let mut state = FileBrowserState::new("/home/user".to_string());
        state.enter_goto();
        assert!(state.goto_mode);
        state.goto_path = "/var".to_string();
        state.exit_goto();
        assert!(!state.goto_mode);
        assert!(state.goto_path.is_empty());
        assert!(state.goto_suggestions.is_empty());
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
