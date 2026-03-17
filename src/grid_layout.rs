use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const MAX_HOSTS_PER_ROW: usize = 10;

/// A row in the grid layout (serializable to YAML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridRow {
    pub name: String,
    pub hosts: Vec<String>,
}

/// Full grid layout (serializable to YAML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridLayout {
    pub rows: Vec<GridRow>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

impl GridLayout {
    /// Create a default layout from a list of host names.
    /// Distributes hosts into rows of up to MAX_HOSTS_PER_ROW.
    pub fn from_hosts(host_names: &[String]) -> Self {
        let mut rows = Vec::new();
        let mut row_idx = 0;

        for chunk in host_names.chunks(MAX_HOSTS_PER_ROW) {
            row_idx += 1;
            rows.push(GridRow {
                name: format!("Row {row_idx}"),
                hosts: chunk.to_vec(),
            });
        }

        // Ensure at least one row
        if rows.is_empty() {
            rows.push(GridRow {
                name: "Row 1".to_string(),
                hosts: Vec::new(),
            });
        }

        GridLayout {
            rows,
            hidden: Vec::new(),
        }
    }

    /// Load layout from a YAML file. Returns None if file doesn't exist or is invalid.
    pub fn load(path: &Path) -> Option<Self> {
        let content = fs::read_to_string(path).ok()?;
        serde_yaml::from_str(&content).ok()
    }

    /// Save layout to a YAML file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_yaml::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(path, content)
    }

    /// Build a layout from the current Dashboard state (for saving).
    pub fn from_dashboard_rows(
        rows: &[(String, Vec<String>)],
        hidden: &[String],
    ) -> Self {
        GridLayout {
            rows: rows
                .iter()
                .map(|(name, hosts)| GridRow {
                    name: name.clone(),
                    hosts: hosts.clone(),
                })
                .collect(),
            hidden: hidden.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_hosts_distributes_correctly() {
        let hosts: Vec<String> = (0..23).map(|i| format!("host-{i}")).collect();
        let layout = GridLayout::from_hosts(&hosts);

        assert_eq!(layout.rows.len(), 3);
        assert_eq!(layout.rows[0].hosts.len(), 10);
        assert_eq!(layout.rows[1].hosts.len(), 10);
        assert_eq!(layout.rows[2].hosts.len(), 3);
        assert_eq!(layout.rows[0].name, "Row 1");
        assert_eq!(layout.rows[2].name, "Row 3");
    }

    #[test]
    fn from_hosts_empty() {
        let layout = GridLayout::from_hosts(&[]);
        assert_eq!(layout.rows.len(), 1);
        assert!(layout.rows[0].hosts.is_empty());
    }

    #[test]
    fn from_hosts_single_row() {
        let hosts: Vec<String> = (0..5).map(|i| format!("host-{i}")).collect();
        let layout = GridLayout::from_hosts(&hosts);
        assert_eq!(layout.rows.len(), 1);
        assert_eq!(layout.rows[0].hosts.len(), 5);
    }

    #[test]
    fn yaml_roundtrip() {
        let layout = GridLayout {
            rows: vec![
                GridRow {
                    name: "Production".to_string(),
                    hosts: vec!["web1".to_string(), "web2".to_string()],
                },
                GridRow {
                    name: "Staging".to_string(),
                    hosts: vec!["staging1".to_string()],
                },
            ],
            hidden: vec!["old-server".to_string()],
        };

        let tmp = PathBuf::from("/tmp/beebo_test_layout.yaml");
        layout.save(&tmp).unwrap();
        let loaded = GridLayout::load(&tmp).unwrap();

        assert_eq!(loaded.rows.len(), 2);
        assert_eq!(loaded.rows[0].name, "Production");
        assert_eq!(loaded.rows[0].hosts, vec!["web1", "web2"]);
        assert_eq!(loaded.rows[1].name, "Staging");
        assert_eq!(loaded.hidden, vec!["old-server"]);

        // Cleanup
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn max_hosts_per_row_is_ten() {
        assert_eq!(MAX_HOSTS_PER_ROW, 10);
    }
}
