use std::sync::Arc;
use tokio::sync::Mutex;

use russh::client;
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;

use crate::disk_info::{self, DiskEntry};
use crate::file_browser::{self, FileBrowserState};
use crate::metrics::{self, MetricsData};
use crate::process_info::{self, ProcessEntry};
use crate::ssh_config::SshHost;
use crate::system_info::{self, SystemInfo};

/// State of an SSH session.
#[derive(Debug, Clone)]
pub enum SessionState {
    Idle,
    Connecting,
    NeedPassword,
    Authenticating,
    Connected,
    Disconnected(String),
}

impl SessionState {
    pub fn label(&self) -> &str {
        match self {
            SessionState::Idle => "Idle",
            SessionState::Connecting => "Connecting...",
            SessionState::NeedPassword => "Password required",
            SessionState::Authenticating => "Authenticating...",
            SessionState::Connected => "Connected",
            SessionState::Disconnected(_) => "Disconnected",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, SessionState::Connected)
    }
}

/// Shared session data accessible from the UI thread.
pub struct SessionData {
    pub state: SessionState,
    pub screen: vt100::Parser,
    pub host: SshHost,
    /// Channel for sending input to the remote PTY.
    pub input_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    /// Real-time system metrics collected over SSH.
    pub metrics: MetricsData,
    /// Static system info (collected once on connect).
    pub system_info: Option<SystemInfo>,
    /// Disk usage entries (collected every 10s).
    pub disks: Option<Vec<DiskEntry>>,
    /// Whether disk data is currently being collected.
    pub disk_loading: bool,
    /// Top processes by CPU (collected every 2s).
    pub processes: Option<Vec<ProcessEntry>>,
    /// SFTP session handle for file browsing.
    pub sftp: Option<Arc<Mutex<SftpSession>>>,
    /// File browser state for this session.
    pub file_browser: FileBrowserState,
}

impl SessionData {
    pub fn new(host: SshHost, cols: u16, rows: u16) -> Self {
        Self {
            state: SessionState::Idle,
            screen: vt100::Parser::new(rows, cols, 200),
            host,
            input_tx: None,
            metrics: MetricsData::new(),
            system_info: None,
            disks: None,
            disk_loading: false,
            processes: None,
            sftp: None,
            file_browser: FileBrowserState::new("/".to_string()),
        }
    }
}

pub type SharedSession = Arc<Mutex<SessionData>>;

/// SSH client handler for russh.
pub struct SshClientHandler;

impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Spawn an async task to connect to a host and manage the SSH session.
pub fn spawn_session(
    session_data: SharedSession,
    rt: tokio::runtime::Handle,
) {
    let rt_clone = rt.clone();
    rt.spawn(async move {
        if let Err(e) = run_session(session_data.clone(), rt_clone).await {
            let mut data = session_data.lock().await;
            data.state = SessionState::Disconnected(format!("{e}"));
        }
    });
}

async fn run_session(session_data: SharedSession, rt: tokio::runtime::Handle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (host_name, port, user, identity_file) = {
        let data = session_data.lock().await;
        let h = data.host.clone();
        (
            h.effective_hostname().to_string(),
            h.effective_port(),
            h.user.clone().unwrap_or_else(|| {
                whoami::username().unwrap_or_else(|_| "root".to_string())
            }),
            h.identity_file.clone(),
        )
    };

    // Update state: Connecting
    {
        let mut data = session_data.lock().await;
        data.state = SessionState::Connecting;
    }

    // Connect
    let config = Arc::new(client::Config::default());
    let handler = SshClientHandler;
    let mut session = client::connect(config, (host_name.as_str(), port), handler).await?;

    // Try key auth first
    let key_authenticated = try_key_auth(&mut session, &user, &identity_file).await;

    if !key_authenticated {
        // Switch to NeedPassword state
        {
            let mut data = session_data.lock().await;
            data.state = SessionState::NeedPassword;
        }

        // Wait for password from UI
        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        {
            let mut data = session_data.lock().await;
            data.input_tx = Some(input_tx);
        }

        let password = match input_rx.recv().await {
            Some(bytes) => String::from_utf8_lossy(&bytes).trim().to_string(),
            None => return Err("Password input cancelled".into()),
        };

        {
            let mut data = session_data.lock().await;
            data.state = SessionState::Authenticating;
        }

        let auth_result = session.authenticate_password(&user, &password).await?;
        if !auth_result.success() {
            let mut data = session_data.lock().await;
            data.state = SessionState::Disconnected("Authentication failed".to_string());
            return Ok(());
        }
    }

    // Open channel and request PTY
    let (cols, rows) = {
        let data = session_data.lock().await;
        let size = data.screen.screen().size();
        (size.1, size.0)
    };

    let mut channel = session.channel_open_session().await?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(false).await?;

    // Wrap session handle in Arc<Mutex> so the metrics collector can share it
    let shared_handle = Arc::new(Mutex::new(session));

    // Spawn metrics collector on separate channels
    metrics::spawn_metrics_collector(shared_handle.clone(), session_data.clone(), rt.clone());

    // Spawn host info collectors
    system_info::spawn_system_info_collector(shared_handle.clone(), session_data.clone(), rt.clone());
    disk_info::spawn_disk_collector(shared_handle.clone(), session_data.clone(), rt.clone());
    process_info::spawn_process_collector(shared_handle.clone(), session_data.clone(), rt);

    // --- SFTP initialization ---
    let home_dir = metrics::exec_remote_cmd(&shared_handle, "echo $HOME")
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "/".to_string());

    let sftp_result = async {
        let h = shared_handle.lock().await;
        let sftp_channel = h.channel_open_session().await?;
        sftp_channel.request_subsystem(true, "sftp").await?;
        Ok::<_, russh::Error>(sftp_channel)
    }
    .await;

    if let Ok(sftp_channel) = sftp_result {
        match SftpSession::new(sftp_channel.into_stream()).await {
            Ok(sftp) => {
                let sftp = Arc::new(Mutex::new(sftp));
                let mut fb_state = FileBrowserState::new(home_dir);
                {
                    let sftp_guard = sftp.lock().await;
                    file_browser::refresh_listing(&sftp_guard, &mut fb_state).await;
                }
                let mut data = session_data.lock().await;
                data.sftp = Some(sftp);
                data.file_browser = fb_state;
            }
            Err(e) => {
                let mut data = session_data.lock().await;
                data.file_browser.error = Some(format!("SFTP init failed: {e}"));
            }
        }
    } else if let Err(e) = sftp_result {
        let mut data = session_data.lock().await;
        data.file_browser.error = Some(format!("SFTP channel failed: {e}"));
    }

    // Set up input channel
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut data = session_data.lock().await;
        data.state = SessionState::Connected;
        data.input_tx = Some(input_tx);
    }

    // Main loop: select between input and output
    loop {
        tokio::select! {
            // User input from the TUI
            Some(input) = input_rx.recv() => {
                channel.data(&input[..]).await?;
            }
            // Remote output from SSH
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        let mut sess = session_data.lock().await;
                        sess.screen.process(&data);
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        let mut sess = session_data.lock().await;
                        sess.screen.process(&data);
                    }
                    Some(ChannelMsg::ExitStatus { .. }) | None => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // Session ended
    {
        let mut data = session_data.lock().await;
        data.state = SessionState::Disconnected("Session ended".to_string());
    }

    Ok(())
}

async fn try_key_auth(
    session: &mut client::Handle<SshClientHandler>,
    user: &str,
    identity_file: &Option<String>,
) -> bool {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut key_paths = Vec::new();

    if let Some(id_file) = identity_file {
        let expanded = if id_file.starts_with("~/") {
            format!("{}/{}", home, &id_file[2..])
        } else {
            id_file.clone()
        };
        key_paths.push(expanded);
    }

    key_paths.push(format!("{home}/.ssh/id_ed25519"));
    key_paths.push(format!("{home}/.ssh/id_rsa"));

    for key_path in &key_paths {
        let path = std::path::Path::new(key_path);
        if !path.exists() {
            continue;
        }

        // Use russh::keys to load (same crate that russh uses internally)
        match russh::keys::load_secret_key(key_path, None) {
            Ok(key) => {
                let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key), None);
                match session.authenticate_publickey(user, key_with_hash).await {
                    Ok(result) if result.success() => return true,
                    _ => continue,
                }
            }
            Err(_) => continue,
        }
    }

    false
}
