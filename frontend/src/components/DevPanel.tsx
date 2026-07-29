// DevPanel — diagnostic dashboard for debugging backend integration.
// All data comes from the Zustand store (populated by loadHealth).
// This page is independent of the rest of the UI — add/remove anything without risk.

import { useCallback, useEffect, useState } from 'react';
import { useStore, type StageSnapshot } from '../store';

export default function DevPanel() {
  const tools = useStore((s) => s.tools);
  const toolCount = useStore((s) => s.toolCount);
  const mcpServers = useStore((s) => s.mcpServers);
  const features = useStore((s) => s.features);
  const llmCount = useStore((s) => s.llmCount);
  const llmProviderIds = useStore((s) => s.llmProviderIds);
  const serverVersion = useStore((s) => s.serverVersion);
  const activeSessions = useStore((s) => s.activeSessions);
  const sandboxShell = useStore((s) => s.sandboxShell);
  const sandboxLevel = useStore((s) => s.sandboxLevel);
  const reconnectMcpServer = useStore((s) => s.reconnectMcpServer);
  const loadHealth = useStore((s) => s.loadHealth);
  const loadSandboxStatus = useStore((s) => s.loadSandboxStatus);

  // Fetch data on mount — independent of session switching
  useEffect(() => {
    loadHealth();
    loadSandboxStatus();
  }, []);

  const featureKeys = Object.keys(features).sort();

  return (
    <div className="flex-1 flex flex-col min-h-0 min-w-0 overflow-y-auto p-6 gap-6"
         style={{scrollbarWidth:'thin',scrollbarColor:'oklch(0.35 0.01 140) transparent'}}>
      {/* ── Header ─────────────────────────────────────────────────── */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-lg font-bold">🔧 Dev Panel</h1>
          <p className="text-xs text-muted-foreground mt-1">
            Diagnostic dashboard — backend integration status
          </p>
        </div>
        <button
          onClick={() => loadHealth()}
          className="px-3 py-1.5 rounded text-xs bg-secondary hover:bg-secondary/80 text-muted-foreground hover:text-foreground transition-colors"
        >
          🔄 Refresh
        </button>
      </div>

      {/* ── Quick Stats Row ─────────────────────────────────────────── */}
      <div className="grid grid-cols-5 gap-3">
        <StatCard label="Server" value={serverVersion || '—'} icon="📦" />
        <StatCard
          label="LLM"
          value={llmCount > 0 ? `✅ ${llmCount} providers` : '❌ Missing'}
          icon="🧠"
          ok={llmCount > 0}
          detail={llmProviderIds.length > 0 ? llmProviderIds.join(', ') : undefined}
        />
        <StatCard label="Tools" value={String(toolCount)} icon="🔧" />
        <StatCard label="Active Sessions" value={String(activeSessions)} icon="📡" />
        <StatCard label="Sandbox" value={`${sandboxShell} (${sandboxLevel})`} icon="🛡️" />
      </div>

      {/* ── Startup Check ────────────────────────────────────────────── */}
      <StartupCheckSection />
      {/* ── Models ────────────────────────────────────────────────── */}
      <ModelsSection />
      {/* ── Memory ─────────────────────────────────────────────────── */}
      <MemorySection />

      {/* ── Context Inspector ────────────────────────────────────────── */}
      <ContextInspector />

      {/* ── MCP Servers ─────────────────────────────────────────────── */}
      <Section title="🔌 MCP Servers" count={mcpServers.length}>
        {mcpServers.length === 0 ? (
          <Empty text="No MCP servers configured" />
        ) : (
          <table className="w-full text-xs">
            <thead>
              <tr className="text-left text-muted-foreground border-b border-border">
                <th className="pb-1.5 font-medium">Name</th>
                <th className="pb-1.5 font-medium">Status</th>
                <th className="pb-1.5 font-medium">Server</th>
                <th className="pb-1.5 font-medium">Tools</th>
                <th className="pb-1.5 font-medium">Actions</th>
              </tr>
            </thead>
            <tbody>
              {mcpServers.map((s) => (
                <tr key={s.name} className="border-b border-border/50">
                  <td className="py-2 font-medium">{s.name}</td>
                  <td className="py-2">
                    <StatusBadge status={s.status} />
                    {s.note && <span className="text-muted-foreground ml-1">({s.note})</span>}
                  </td>
                  <td className="py-2 text-muted-foreground">{s.server || '—'}</td>
                  <td className="py-2 text-muted-foreground">
                    {s.tools ?? 0}
                    {s.tool_names && s.tool_names.length > 0 && (
                      <span className="ml-1 text-[10px] opacity-60">
                        ({s.tool_names.slice(0, 5).join(', ')}{s.tool_names.length > 5 ? '…' : ''})
                      </span>
                    )}
                  </td>
                  <td className="py-2">
                    <button
                      onClick={() => reconnectMcpServer(s.name)}
                      className="text-[10px] px-2 py-0.5 rounded bg-secondary hover:bg-secondary/80 transition-colors"
                    >
                      Reconnect
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>

      {/* ── Tools List ──────────────────────────────────────────────── */}
      <Section title="🔧 Tools" count={tools.length}>
        {tools.length === 0 ? (
          <Empty text="No tools loaded — click Refresh" />
        ) : (
          <div className="grid grid-cols-2 gap-2">
            {tools.map((t) => (
              <div key={t.name} className="flex items-start gap-2 bg-secondary/50 rounded-md px-3 py-2">
                <code className="text-xs font-bold text-primary shrink-0">{t.name}</code>
                <span className="text-xs text-muted-foreground leading-relaxed flex-1">{t.description}</span>
                <span
                  className={`shrink-0 text-[10px] px-1.5 py-0.5 rounded-full ${
                    t.source === 'builtin'
                      ? 'bg-primary/10 text-primary'
                      : 'bg-warning/10 text-warning'
                  }`}
                >
                  {t.source}
                </span>
              </div>
            ))}
          </div>
        )}
      </Section>

      {/* ── Features ────────────────────────────────────────────────── */}
      <Section title="✨ Feature Flags" count={featureKeys.length}>
        <div className="flex flex-wrap gap-1.5">
          {featureKeys.map((k) => {
            const v = features[k];
            const label = typeof v === 'boolean'
              ? k
              : `${k}: ${Array.isArray(v) ? (v as string[]).join(', ') : String(v)}`;
            return (
              <span
                key={k}
                className={`text-[10px] px-2 py-0.5 rounded-full ${
                  v === true || (Array.isArray(v) && v.length > 0)
                    ? 'bg-success/10 text-success border border-success/30'
                    : 'bg-secondary text-muted-foreground'
                }`}
              >
                {label}
              </span>
            );
          })}
        </div>
      </Section>
    </div>
  );
}

// ── Mini components ───────────────────────────────────────────────────────

function Section({ title, count, children }: { title: string; count: number; children: React.ReactNode }) {
  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <div className="bg-secondary/50 px-4 py-2 flex items-center gap-2 border-b border-border">
        <span className="text-sm font-medium">{title}</span>
        <span className="text-[10px] text-muted-foreground bg-secondary rounded-full px-1.5 py-0.5">
          {count}
        </span>
      </div>
      <div className="p-3">{children}</div>
    </div>
  );
}

function StatCard({ label, value, icon, ok, detail }: { label: string; value: string; icon: string; ok?: boolean; detail?: string }) {
  return (
    <div className="border border-border rounded-lg p-3 bg-card hover:border-primary/30 transition-colors">
      <div className="text-xs text-muted-foreground mb-1">{icon} {label}</div>
      <div className={`text-sm font-semibold ${ok === false ? 'text-destructive' : 'text-foreground'}`}>
        {value}
      </div>
      {detail && <div className="text-[10px] text-muted-foreground mt-0.5 truncate">{detail}</div>}
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const colors: Record<string, string> = {
    connected: 'bg-success/10 text-success border-success/30',
    dead: 'bg-destructive/10 text-destructive border-destructive/30',
    busy: 'bg-warning/10 text-warning border-warning/30',
  };
  return (
    <span className={`text-[10px] px-1.5 py-0.5 rounded-full border ${colors[status] || 'bg-secondary text-muted-foreground'}`}>
      {status}
    </span>
  );
}

function Empty({ text }: { text: string }) {
  return <p className="text-xs text-muted-foreground italic">{text}</p>;
}

function ModelsSection() {
  const [models, setModels] = useState<any>(null);
  const [reindexMsg, setReindexMsg] = useState('');

  const fetchModels = useCallback(async () => {
    try {
      const res = await fetch('/api/models');
      setModels(await res.json());
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { fetchModels(); }, [fetchModels]);

  const activateModel = async (name: string) => {
    try {
      await fetch('/api/models/activate', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: name }),
      });
      setTimeout(fetchModels, 1000);
    } catch { /* ignore */ }
  };

  const triggerReindex = async () => {
    setReindexMsg('Reindexing...');
    try {
      const res = await fetch('/api/vector/reindex', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ collection: 'memory' }),
      });
      const data = await res.json();
      setReindexMsg(`Done: ${data.processed} facts in ${data.duration_ms}ms`);
      setTimeout(() => setReindexMsg(''), 5000);
    } catch { setReindexMsg('Failed'); }
  };

  if (!models) return null;

  return (
    <Section title="🧠 Embedding Models" count={models.models?.length || 0}>
      <div className="grid grid-cols-1 gap-2 mb-3">
        {models.models?.map((m: any) => (
          <div key={m.name} className={`flex items-center justify-between px-3 py-2 rounded border ${m.active ? 'border-primary/50 bg-primary/5' : 'border-border bg-secondary/30'}`}>
            <div>
              <span className="text-sm font-medium">{m.display_name}</span>
              <span className="text-xs text-muted-foreground ml-2">{m.dim}d</span>
              {m.active && <span className="text-[10px] ml-2 px-1 py-0.5 rounded bg-primary/20 text-primary">active</span>}
            </div>
            {!m.active && (
              <button onClick={() => activateModel(m.name)}
                className="text-[10px] px-2 py-1 rounded bg-secondary hover:bg-secondary/80">
                Switch
              </button>
            )}
          </div>
        ))}
      </div>
      <div className="flex items-center gap-2">
        <button onClick={triggerReindex}
          className="text-[10px] px-2 py-1 rounded bg-accent/20 hover:bg-accent/30">
          🔄 Reindex (current model)
        </button>
        {reindexMsg && <span className="text-[10px] text-muted-foreground">{reindexMsg}</span>}
      </div>
    </Section>
  );
}

function MemorySection() {
  const [memStatus, setMemStatus] = useState<any>(null);
  const [loading, setLoading] = useState(false);

  const fetchMemory = useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch('/api/memory/status');
      setMemStatus(await res.json());
    } catch { /* ignore */ }
    setLoading(false);
  }, []);

  useEffect(() => { fetchMemory(); }, [fetchMemory]);

  const triggerDream = async (phase: string) => {
    try {
      await fetch('/api/memory/dream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ phase }),
      });
      setTimeout(fetchMemory, 2000);
    } catch { /* ignore */ }
  };

  const p = memStatus?.pipeline || {};
  const s = memStatus?.storage || {};

  return (
    <Section title="🧠 Memory" count={s.facts || 0}>
      <div className="grid grid-cols-4 gap-2 mb-3">
        <StatCard label="Facts" value={String(s.facts ?? '?')} icon="📝" />
        <StatCard label="Diary" value={String(s.diary_files ?? '?')} icon="📖" />
        <StatCard label="Wiki" value={String(s.wiki_pages ?? '?')} icon="📚" />
        <StatCard label="KG" value={s.graph_exists ? '✓' : '✗'} icon="🕸️" ok={s.graph_exists} />
      </div>
      <div className="grid grid-cols-3 gap-2 mb-3">
        <StatCard label="LLM" value={p.has_llm ? '✓' : '✗'} icon="🤖" ok={p.has_llm} />
        <StatCard label="Buffered" value={p.buffered_messages ? 'Yes' : 'No'} icon="📥" />
        <StatCard label="Scheduler" value={p.scheduler_running ? 'On' : 'Off'} icon="⏰" ok={p.scheduler_running} />
      </div>
      <div className="flex gap-2">
        <button onClick={() => triggerDream('light')} disabled={loading}
          className="text-[10px] px-2 py-1 rounded bg-secondary hover:bg-secondary/80 disabled:opacity-50">
          🌅 LIGHT
        </button>
        <button onClick={() => triggerDream('all')} disabled={loading}
          className="text-[10px] px-2 py-1 rounded bg-primary/20 hover:bg-primary/30 disabled:opacity-50">
          🔄 Full Pipeline
        </button>
        <button onClick={fetchMemory} disabled={loading}
          className="text-[10px] px-2 py-1 rounded bg-secondary hover:bg-secondary/80 disabled:opacity-50">
          🔄 Refresh
        </button>
      </div>
    </Section>
  );
}

function StartupCheckSection() {
  const report = useStore((s) => s.startupReport);
  if (!report) return null;

  const icon = (s: string) => s === 'pass' ? '✅' : s === 'warn' ? '⚠️' : '❌';
  const color = (s: string) => s === 'pass' ? 'text-success' : s === 'warn' ? 'text-warning' : 'text-destructive';

  return (
    <Section title="🩺 Startup Check" count={report.checks?.length || 0}>
      <div className="grid grid-cols-4 gap-2 mb-3">
        <StatCard label="Pass" value={String(report.pass)} icon="✅" ok={true} />
        <StatCard label="Warn" value={String(report.warn)} icon="⚠️" ok={report.warn === 0} />
        <StatCard label="Fail" value={String(report.fail)} icon="❌" ok={report.fail === 0} />
        <StatCard label="Total" value={`${report.total_ms}ms`} icon="⏱️" />
      </div>
      <div className="text-xs space-y-1 max-h-64 overflow-y-auto">
        {report.checks?.map((c: any, i: number) => (
          <div key={i} className={`flex items-center gap-2 px-2 py-1 rounded ${c.status === 'fail' ? 'bg-destructive/10' : c.status === 'warn' ? 'bg-warning/5' : 'bg-secondary/30'}`}>
            <span className="shrink-0">{icon(c.status)}</span>
            <span className={`font-medium ${color(c.status)}`}>{c.name}</span>
            <span className="text-muted-foreground flex-1 truncate">{c.detail}</span>
            <span className="text-[10px] text-muted-foreground shrink-0">{c.latency_ms}ms</span>
          </div>
        ))}
      </div>
      {report.actual_port && (
        <div className="text-[10px] text-muted-foreground mt-2">
          Server port: {report.actual_port}
        </div>
      )}
    </Section>
  );
}

// ── Context Inspector ───────────────────────────────────────────────────────

function ContextInspector() {
  const snapshot = useStore((s) => s.contextSnapshot);
  const loading = useStore((s) => s.contextSnapshotLoading);
  const loadContextSnapshot = useStore((s) => s.loadContextSnapshot);
  const activeSessionId = useStore((s) => s.activeSessionId);

  const [expandedStages, setExpandedStages] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (activeSessionId) loadContextSnapshot();
  }, [activeSessionId]);

  const toggleExpand = (name: string) => {
    setExpandedStages(prev => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  if (!snapshot) {
    return (
      <Section title="🔍 Context Inspector" count={0}>
        <Empty text={activeSessionId
          ? "No context snapshot yet — send a message first"
          : "Select a session to inspect context"
        } />
        {activeSessionId && (
          <button
            onClick={loadContextSnapshot}
            className="mt-2 text-[10px] px-2 py-1 rounded bg-secondary hover:bg-secondary/80 transition-colors"
          >
            {loading ? 'Loading...' : '🔄 Fetch'}
          </button>
        )}
      </Section>
    );
  }

  const sortedStages = [...snapshot.stages].sort((a, b) => a.priority - b.priority);
  const budgetPct = Math.min(snapshot.budget_used_pct, 100);
  const gaugeColor = budgetPct > 100 ? 'bg-destructive'
    : budgetPct > 80 ? 'bg-warning'
    : 'bg-success';

  return (
    <Section title="🔍 Context Inspector" count={snapshot.stages.length}>
      {/* Budget gauge */}
      <div className="mb-3">
        <div className="flex justify-between text-xs mb-1">
          <span className="text-muted-foreground">Context Budget</span>
          <span className="font-medium">
            ~{snapshot.total_estimated_tokens.toLocaleString()} / {snapshot.max_context_tokens.toLocaleString()} tokens
          </span>
        </div>
        <div className="w-full h-2 bg-secondary rounded overflow-hidden">
          <div
            className={`h-full ${gaugeColor} transition-all duration-300`}
            style={{ width: `${budgetPct}%` }}
          />
        </div>
        <div className="text-[10px] text-muted-foreground mt-0.5">
          Turn #{snapshot.turn_number} • {snapshot.captured_at}
        </div>
      </div>

      {/* Flags */}
      {snapshot.flags.length > 0 && (
        <div className="mb-3 p-2 bg-destructive/10 border border-destructive/30 rounded">
          <div className="text-xs font-medium text-destructive mb-1">⚠ Detected Issues</div>
          {snapshot.flags.map((f, i) => (
            <div key={i} className="text-[11px] text-muted-foreground leading-relaxed">• {f}</div>
          ))}
        </div>
      )}

      {/* Per-stage rows */}
      <div className="space-y-1">
        {sortedStages.map((stage: StageSnapshot) => {
          const pct = snapshot.max_context_tokens > 0
            ? (stage.estimated_tokens / snapshot.max_context_tokens) * 100
            : 0;
          const barColor = stage.status === 'missing' ? 'bg-destructive'
            : stage.status === 'oversized' || stage.status === 'warn' ? 'bg-warning'
            : 'bg-success';
          const rowBg = stage.status === 'missing' ? 'bg-destructive/10'
            : stage.status === 'oversized' ? 'bg-warning/5'
            : stage.status === 'warn' ? 'bg-warning/5'
            : 'bg-secondary/30';
          const isExpanded = expandedStages.has(stage.stage_name);

          return (
            <div key={stage.stage_name} className={`rounded px-2 py-1.5 ${rowBg}`}>
              {/* Row header */}
              <div
                className="flex items-center gap-2 cursor-pointer"
                onClick={() => stage.content_preview && toggleExpand(stage.stage_name)}
              >
                <span className="shrink-0 text-xs">
                  {stage.status === 'missing' ? '❌'
                    : stage.status === 'oversized' ? '⚠️'
                    : stage.status === 'warn' ? '⚠️'
                    : '✅'}
                </span>
                <div className="flex-1 min-w-0">
                  <div className="text-xs font-medium truncate">
                    {stage.stage_name}
                    {stage.label && <span className="text-muted-foreground ml-1">{stage.label}</span>}
                  </div>
                </div>
                <span className="text-[10px] text-muted-foreground shrink-0">
                  ~{stage.estimated_tokens} tok • P{stage.priority}
                </span>
              </div>

              {/* Percentage bar */}
              {stage.contributed && pct > 0 && (
                <div className="w-full h-1 bg-secondary rounded overflow-hidden mt-1">
                  <div
                    className={`h-full ${barColor} transition-all`}
                    style={{ width: `${Math.min(pct, 100)}%` }}
                  />
                </div>
              )}

              {/* Status text */}
              <div className="text-[10px] mt-0.5">
                {!stage.contributed && (
                  <span className="text-warning">Skipped — stage returned no content</span>
                )}
                {stage.contributed && (
                  <span className={stage.status === 'ok' ? 'text-success' : stage.status === 'oversized' ? 'text-warning' : 'text-muted-foreground'}>
                    {stage.message_count} message{stage.message_count !== 1 ? 's' : ''}
                    {stage.status === 'oversized' && ` • ${pct.toFixed(0)}% of budget`}
                  </span>
                )}
              </div>

              {/* Expandable content preview */}
              {isExpanded && stage.content_preview && (
                <div className="mt-1 p-2 bg-background/50 rounded border border-border/50">
                  <pre className="text-[10px] leading-relaxed whitespace-pre-wrap break-all max-h-48 overflow-y-auto text-muted-foreground font-mono">
                    {stage.content_preview.length > 500
                      ? stage.content_preview.slice(0, 500) + '\n\n... (truncated for display)'
                      : stage.content_preview}
                  </pre>
                </div>
              )}
            </div>
          );
        })}
      </div>

      <button
        onClick={loadContextSnapshot}
        className="mt-3 text-[10px] px-2 py-1 rounded bg-secondary hover:bg-secondary/80 transition-colors"
      >
        {loading ? 'Loading...' : '🔄 Refresh'}
      </button>
    </Section>
  );
}
