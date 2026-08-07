//! Manifest files — track installed runtime and model versions.
//!
//! Format: `data/{category}/.manifest.json`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A single entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub version: String,
    pub installed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Top-level manifest structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: HashMap<String, ManifestEntry>,
}

impl Manifest {
    /// Load a manifest from a JSON file. Returns an empty manifest if the file doesn't exist.
    pub async fn load(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self {
                entries: HashMap::new(),
            });
        }
        let json = tokio::fs::read_to_string(path).await?;
        Ok(serde_json::from_str(&json).unwrap_or_else(|_| Self {
            entries: HashMap::new(),
        }))
    }

    /// Save the manifest to a JSON file.
    pub async fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(path, json).await?;
        Ok(())
    }

    /// Get an entry by key.
    pub fn get(&self, key: &str) -> Option<&ManifestEntry> {
        self.entries.get(key)
    }

    /// Insert or update an entry.
    pub fn upsert(&mut self, key: &str, version: &str, sha256: Option<&str>) {
        self.entries.insert(
            key.to_string(),
            ManifestEntry {
                version: version.to_string(),
                installed_at: chrono::Utc::now().to_rfc3339(),
                sha256: sha256.map(|s| s.to_string()),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_empty_manifest_load() {
        let manifest = Manifest::load(Path::new("/nonexistent/path.json"))
            .await
            .unwrap();
        assert!(manifest.entries.is_empty());
    }

    #[test]
    fn test_upsert_and_get() {
        let mut manifest = Manifest {
            entries: HashMap::new(),
        };
        manifest.upsert("python", "3.12.8", Some("abc123"));
        let entry = manifest.get("python").unwrap();
        assert_eq!(entry.version, "3.12.8");
        assert_eq!(entry.sha256.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let mut manifest = Manifest {
            entries: HashMap::new(),
        };
        manifest.upsert("node", "22.12.0", None);
        let tmp = NamedTempFile::new().unwrap();
        manifest.save(tmp.path()).await.unwrap();
        let loaded = Manifest::load(tmp.path()).await.unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.get("node").unwrap().version, "22.12.0");
    }

    #[tokio::test]
    async fn test_load_broken_json_returns_empty() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "not json").unwrap();
        let manifest = Manifest::load(tmp.path()).await.unwrap();
        assert!(manifest.entries.is_empty());
    }
}
