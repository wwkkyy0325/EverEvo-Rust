//! Plugin Rollback — emergency rollback any plugin to its stable version.
//! Uses PluginRegistry for real rollback operations.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use crate::plugin::registry::PluginRegistry;

pub struct PluginRollback {
    registry: Option<Arc<PluginRegistry>>,
}

impl PluginRollback {
    pub fn new(registry: Option<Arc<PluginRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PluginRollback {
    fn name(&self) -> &str { "plugin_rollback" }
    fn description(&self) -> &str {
        "Emergency rollback any plugin to its last stable version. \
         Kills canary processes and resets canary traffic to 0. \
         Parameters: { \"plugin_id\": string }. \
         Use this when a plugin is broken and needs immediate recovery. \
         The stable version is NOT affected — only the canary is removed."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "plugin_id": {
                    "type": "string",
                    "description": "Plugin ID to rollback (required)"
                }
            },
            "required": ["plugin_id"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let plugin_id = params["plugin_id"]
            .as_str()
            .ok_or_else(|| EverEvoError::Tool {
                tool: "plugin_rollback".into(),
                message: "Missing 'plugin_id' parameter".into(),
            })?;

        let registry = match &self.registry {
            Some(r) => r,
            None => {
                return Ok(ToolOutput {
                    content: "Plugin registry not initialized — cannot rollback.".into(),
                    is_error: true,
                    ..Default::default()
                });
            }
        };

        // Load current config to report what we're rolling back
        let config = match registry.store().load_config(plugin_id) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Plugin '{plugin_id}' not found: {e}"),
                    is_error: true,
                    ..Default::default()
                });
            }
        };

        let canary_ver = match config.canary.clone() {
            Some(v) => v,
            None => {
                return Ok(ToolOutput::text(format!(
                    "Plugin '{plugin_id}' has no active canary — nothing to rollback. Current stable: {}",
                    config.stable
                )));
            }
        };

        // Perform rollback
        match registry.rollback(plugin_id) {
            Ok(()) => {
                let mut result = format!(
                    "✅ Rollback successful for '{plugin_id}'\n\
                     Removed canary: {canary_ver}\n\
                     Stable remains: {}\n\
                     All traffic now routed to stable.\n",
                    config.stable
                );

                // Report metrics for audit trail
                if let Some(metrics) = config.metrics.get(&canary_ver) {
                    result.push_str(&format!(
                        "\nCanary metrics at rollback time:\n\
                         Success rate: {:.1}%\n\
                         Total calls: {}\n\
                         Crashes: {}\n\
                         Avg latency: {:.0}ms\n",
                        metrics.success_rate() * 100.0,
                        metrics.total_count,
                        metrics.crash_count,
                        metrics.avg_latency_ms(),
                    ));
                }

                Ok(ToolOutput::text(result))
            }
            Err(e) => Ok(ToolOutput {
                content: format!("Rollback failed: {e}"),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}
