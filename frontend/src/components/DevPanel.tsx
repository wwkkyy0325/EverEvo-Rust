// DevPanel — diagnostic dashboard for debugging backend integration.
// All data comes from the Zustand store (populated by loadHealth).
// This page is independent of the rest of the UI — add/remove anything without risk.

import { useCallback, useEffect, useState } from 'react';
import { useStore } from '../store';

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
    <div className="flex-1 flex flex-col min-h-0 min-w-0 overflow-y-auto scrollbar-none p-6 gap-6">
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

      {/* ── Memory ─────────────────────────────────────────────────── */}
      <MemorySection />

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
