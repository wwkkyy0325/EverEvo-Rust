//! Plugin Status + Canary Management — query health, manage canary deployments,
//! promote/rollback versions, and evaluate canary metrics.
//! Uses PluginRegistry for all operations.

use std::sync::Arc;

use async_trait::async_trait;
use everevo_core::tool::{Tool, ToolOutput};
use everevo_core::types::RiskLevel;
use everevo_core::EverEvoError;
use tokio_util::sync::CancellationToken;

use crate::plugin::canary::PromoteDecision;
use crate::plugin::registry::PluginRegistry;

pub struct PluginStatus {
    registry: Option<Arc<PluginRegistry>>,
}

impl PluginStatus {
    pub fn new(registry: Option<Arc<PluginRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PluginStatus {
    fn name(&self) -> &str { "plugin_status" }
    fn description(&self) -> &str {
        "Check plugin health, manage canary deployments, promote/rollback versions. \
         Actions: 'status' (default, shows metrics and versions), \
         'set_canary' (deploy a version as canary with N% traffic), \
         'promote' (promote canary to stable), \
         'evaluate' (let CanaryRouter decide whether to promote/rollback based on metrics). \
         Parameters: { \"action\"?: string, \"plugin_id\": string, \
         \"version\"?: string (for set_canary/promote), \
         \"pct\"?: number (canary traffic %, e.g. 0.1 = 10%) }"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "set_canary", "promote", "evaluate"],
                    "description": "Action: 'status' (view), 'set_canary' (deploy canary), 'promote' (canary→stable), 'evaluate' (auto-decide)"
                },
                "plugin_id": {
                    "type": "string",
                    "description": "Plugin ID (required for set_canary/promote/evaluate; omit for fleet-wide status)"
                },
                "version": {
                    "type": "string",
                    "description": "Version to set as canary or promote (e.g. '1.0.1')"
                },
                "pct": {
                    "type": "number",
                    "description": "Canary traffic percentage (0.0-1.0, default 0.1 = 10%)"
                }
            },
            "required": []
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }

    async fn execute(
        &self,
        params: serde_json::Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<ToolOutput, EverEvoError> {
        let registry = match &self.registry {
            Some(r) => r,
            None => return Ok(ToolOutput::text(
                "Plugin registry not initialized — plugin_status unavailable."
            )),
        };

        let action = params["action"].as_str().unwrap_or("status");

        match action {
            "status" => self.handle_status(registry, &params),
            "set_canary" => self.handle_set_canary(registry, &params),
            "promote" => self.handle_promote(registry, &params),
            "evaluate" => self.handle_evaluate(registry, &params),
            _ => Ok(ToolOutput {
                content: "Unknown action. Use 'status', 'set_canary', 'promote', or 'evaluate'.".into(),
                is_error: true,
                ..Default::default()
            }),
        }
    }
}

impl PluginStatus {
    fn handle_status(
        &self,
        registry: &Arc<PluginRegistry>,
        params: &serde_json::Value,
    ) -> Result<ToolOutput, EverEvoError> {
        if let Some(plugin_id) = params["plugin_id"].as_str() {
            match registry.store().load_config(plugin_id) {
                Ok(config) => {
                    let versions = registry.store().list_versions(plugin_id).unwrap_or_default();
                    let canary_info = match &config.canary {
                        Some(v) => format!("canary: {v} ({}% traffic)", (config.canary_pct * 100.0) as u32),
                        None => "no active canary".into(),
                    };
                    let mut result = format!(
                        "Plugin: {plugin_id}\n  Stable: {}\n  {canary_info}\n  Versions: {}\n",
                        config.stable,
                        versions.join(", ")
                    );
                    for (ver, metrics) in &config.metrics {
                        result.push_str(&format!(
                            "  Metrics {ver}: success={:.1}% ({} calls), avg_lat={:.0}ms, crashes={}\n",
                            metrics.success_rate() * 100.0,
                            metrics.total_count,
                            metrics.avg_latency_ms(),
                            metrics.crash_count,
                        ));
                    }
                    Ok(ToolOutput::text(result))
                }
                Err(e) => Ok(ToolOutput {
                    content: format!("Plugin '{plugin_id}' not found: {e}"),
                    is_error: true,
                    ..Default::default()
                }),
            }
        } else {
            let plugins_dir = registry.plugins_dir();
            let mut result = String::from("Plugin Fleet Status\n==================\n\n");
            if let Ok(entries) = std::fs::read_dir(plugins_dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let id = entry.file_name().to_string_lossy().into_owned();
                        match registry.store().load_config(&id) {
                            Ok(config) => {
                                let m = config.metrics.get(&config.stable);
                                let success = m.map(|m| format!("{:.1}%", m.success_rate() * 100.0))
                                    .unwrap_or_else(|| "N/A".into());
                                let canary_note = config.canary.as_ref()
                                    .map(|v| format!(" [canary:{v} @{}%]", (config.canary_pct*100.0) as u32))
                                    .unwrap_or_default();
                                result.push_str(&format!(
                                    "  {id}: stable={}, success={success}{canary_note}\n",
                                    config.stable
                                ));
                            }
                            Err(_) => {
                                result.push_str(&format!("  {id}: no config\n"));
                            }
                        }
                    }
                }
            }
            Ok(ToolOutput::text(result))
        }
    }

    fn handle_set_canary(
        &self,
        registry: &Arc<PluginRegistry>,
        params: &serde_json::Value,
    ) -> Result<ToolOutput, EverEvoError> {
        let plugin_id = params["plugin_id"].as_str().unwrap_or("");
        let version = params["version"].as_str().unwrap_or("");
        let pct = params["pct"].as_f64().unwrap_or(0.1);
        if plugin_id.is_empty() || version.is_empty() {
            return Ok(ToolOutput {
                content: "Provide 'plugin_id', 'version', and optionally 'pct' for set_canary.".into(),
                is_error: true,
                ..Default::default()
            });
        }
        registry
            .store()
            .set_canary(plugin_id, version, pct)
            .map_err(|e| EverEvoError::Internal(format!("set_canary: {e}")))?;
        Ok(ToolOutput::text(format!(
            "Canary set: {plugin_id} → version {version} at {}% traffic.\n\
             Monitor with plugin_status(action='evaluate', plugin_id='{plugin_id}').\n\
             Promote with plugin_status(action='promote', plugin_id='{plugin_id}', version='{version}').",
            (pct * 100.0) as u32
        )))
    }

    fn handle_promote(
        &self,
        registry: &Arc<PluginRegistry>,
        params: &serde_json::Value,
    ) -> Result<ToolOutput, EverEvoError> {
        let plugin_id = params["plugin_id"].as_str().unwrap_or("");
        let version = params["version"].as_str().unwrap_or("");
        if plugin_id.is_empty() {
            return Ok(ToolOutput {
                content: "Provide 'plugin_id' for promote (version optional — promotes current canary).".into(),
                is_error: true,
                ..Default::default()
            });
        }
        // Verify there IS an active canary before promoting
        let config = registry.store().load_config(plugin_id)
            .map_err(|e| EverEvoError::Internal(format!("load_config: {e}")))?;
        let canary_ver = config.canary
            .ok_or_else(|| EverEvoError::Internal(format!("No active canary for '{plugin_id}'. Use set_canary first.")))?;
        // If a specific version was requested, verify it matches the active canary
        if !version.is_empty() && version != canary_ver {
            return Ok(ToolOutput {
                content: format!("Version '{version}' is not the active canary. Current canary is '{canary_ver}'."),
                is_error: true,
                ..Default::default()
            });
        }
        registry
            .store()
            .promote(plugin_id)
            .map_err(|e| EverEvoError::Internal(format!("promote: {e}")))?;
        Ok(ToolOutput::text(format!(
            "Promoted {plugin_id} canary to stable. Verify with plugin_status(action='status', plugin_id='{plugin_id}')."
        )))
    }

    fn handle_evaluate(
        &self,
        registry: &Arc<PluginRegistry>,
        params: &serde_json::Value,
    ) -> Result<ToolOutput, EverEvoError> {
        let plugin_id = params["plugin_id"].as_str().unwrap_or("");
        if plugin_id.is_empty() {
            return Ok(ToolOutput {
                content: "Provide 'plugin_id' for evaluate (e.g., plugin_status(action='evaluate', plugin_id='web_search')).".into(),
                is_error: true,
                ..Default::default()
            });
        }
        let decision = registry
            .router()
            .evaluate(plugin_id)
            .map_err(|e| EverEvoError::Internal(format!("evaluate: {e}")))?;

        // Also gather metrics for the report
        let config = registry.store().load_config(plugin_id)
            .map_err(|e| EverEvoError::Internal(format!("load_config: {e}")))?;
        let stable_m = config.metrics.get(&config.stable);
        let canary_m = config.canary.as_ref().and_then(|v| config.metrics.get(v));

        let mut report = match &decision {
            PromoteDecision::Promote => format!(
                "✓ PROMOTE: {plugin_id} canary is ready for promotion.\n"
            ),
            PromoteDecision::Rollback => format!(
                "✗ ROLLBACK: {plugin_id} canary should be rolled back.\n"
            ),
            PromoteDecision::KeepObserving => format!(
                "⟳ KEEP OBSERVING: {plugin_id} canary metrics are within tolerance.\n"
            ),
            PromoteDecision::InsufficientData => format!(
                "? INSUFFICIENT DATA: {plugin_id} needs ≥100 samples for evaluation.\n"
            ),
            PromoteDecision::Observing => format!(
                "⏳ OBSERVING: {plugin_id} has enough samples but needs more time.\n"
            ),
            PromoteDecision::NoCanary => format!(
                "○ NO CANARY: {plugin_id} has no active canary.\n"
            ),
        };

        if let (Some(sm), Some(cm)) = (stable_m, canary_m) {
            report.push_str(&format!(
                "\n  Stable:  success={:.1}% ({:.0}ms avg)\n  Canary:  success={:.1}% ({:.0}ms avg)\n  Delta:   {:.1}% success, {:.0}ms latency\n",
                sm.success_rate() * 100.0, sm.avg_latency_ms(),
                cm.success_rate() * 100.0, cm.avg_latency_ms(),
                (cm.success_rate() - sm.success_rate()) * 100.0,
                cm.avg_latency_ms() - sm.avg_latency_ms(),
            ));
        }

        report.push_str(&format!(
            "\nAction: {}",
            match decision {
                PromoteDecision::Promote =>
                    "Run plugin_status(action='promote', plugin_id='{plugin_id}') to finalize.",
                PromoteDecision::Rollback =>
                    "Run plugin_rollback(plugin_id='{plugin_id}') to revert.",
                PromoteDecision::KeepObserving | PromoteDecision::Observing =>
                    "Wait and re-evaluate later.",
                PromoteDecision::InsufficientData =>
                    "Wait for more traffic before evaluating.",
                PromoteDecision::NoCanary =>
                    "Run plugin_status(action='set_canary', ...) to start a canary deployment.",
            }
        ));

        Ok(ToolOutput::text(report))
    }
}
