// Routing config — first-try-cheap escalation cascade.
//
// 2026 production consensus (BitRouter, ModelCascade, CRC Router, Claude Code):
//   Start with cheapest model, escalate only on failure.
//   Agent (LLM) decides when to escalate — no keyword matcher, no pre-classifier.

import { useState, useCallback, useEffect } from 'react';

export type EffortLevel = 'auto' | 'off' | 'high' | 'max';

export interface TierConfig {
  modelId: string;
  effort: EffortLevel;
}

export interface RoutingConfig {
  mainModelId: string;          // orchestrator — plans, decides, chats
  mainEffort: EffortLevel;      // orchestrator thinking depth
  visionModelId: string;        // image description provider (describe_image) — '' = off
  compactModelId: string;       // context compaction/rolling-summary provider — '' = main model
  metaAgentEnabled: boolean;    // meta-agent self-diagnosis — product ON, benchmark OFF
  tiers: [TierConfig, TierConfig, TierConfig]; // sub-agent execution cascade
}

const STORAGE_KEY = 'everevo_routing_v8';

const DEFAULT: RoutingConfig = {
  mainModelId: '',
  mainEffort: 'auto',
  visionModelId: '',
  compactModelId: '',
  metaAgentEnabled: true,
  tiers: [
    { modelId: '', effort: 'auto' },
    { modelId: '', effort: 'high' },
    { modelId: '', effort: 'max' },
  ],
};

function loadLocal(): RoutingConfig {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return { ...DEFAULT, ...JSON.parse(raw) };
  } catch { /* ignore */ }
  return DEFAULT;
}

function persist(config: RoutingConfig) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(config));
  fetch('/api/routing', { method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(config),
  }).catch(() => {});
}

export function useRoutingConfig() {
  const [config, setConfig] = useState<RoutingConfig>(loadLocal);
  const [synced, setSynced] = useState(false);

  useEffect(() => {
    fetch('/api/routing').then(r => r.json()).then(data => {
      if (data.tiers) {
        const tiers = data.tiers.map((t: any) => ({
          modelId: t.modelId || '', effort: t.effort || 'auto',
        }));
        if (tiers.length === 3) {
          const remote = {
            mainModelId: data.mainModelId || '',
            mainEffort: data.mainEffort || 'auto',
            visionModelId: data.visionModelId || '',
            compactModelId: data.compactModelId || '',
            metaAgentEnabled: data.metaAgentEnabled !== false,
            tiers: tiers as [TierConfig, TierConfig, TierConfig],
          };
          setConfig(remote);
          localStorage.setItem(STORAGE_KEY, JSON.stringify(remote));
        }
      }
    }).catch(() => {}).finally(() => setSynced(true));
  }, []);

  const setMainModel = useCallback((modelId: string) => {
    setConfig(prev => { const next = { ...prev, mainModelId: modelId }; persist(next); return next; });
  }, []);

  const setMainEffort = useCallback((effort: EffortLevel) => {
    setConfig(prev => { const next = { ...prev, mainEffort: effort }; persist(next); return next; });
  }, []);

  const setTier = useCallback((index: number, field: 'modelId' | 'effort', value: string) => {
    setConfig(prev => {
      const tiers = [...prev.tiers] as [TierConfig, TierConfig, TierConfig];
      tiers[index] = { ...tiers[index], [field]: value };
      const next = { ...prev, tiers };
      persist(next);
      return next;
    });
  }, []);

  const setVisionModel = useCallback((modelId: string) => {
    setConfig(prev => { const next = { ...prev, visionModelId: modelId }; persist(next); return next; });
  }, []);

  const setCompactModel = useCallback((modelId: string) => {
    setConfig(prev => { const next = { ...prev, compactModelId: modelId }; persist(next); return next; });
  }, []);

  const setMetaAgentEnabled = useCallback((enabled: boolean) => {
    setConfig(prev => { const next = { ...prev, metaAgentEnabled: enabled }; persist(next); return next; });
  }, []);

  return { config, synced, setMainModel, setMainEffort, setTier, setVisionModel, setCompactModel, setMetaAgentEnabled };
}
