import { useState, useEffect } from 'react';
import { useRoutingConfig, type EffortLevel } from '../hooks/useRoutingConfig';

// ── Types ───────────────────────────────────────────────────────────────

interface Provider {
  id: string;
  api_format: string;
  api_key: string;
  base_url: string;
  model: string;
}

type SettingsTab = 'llm' | 'routing';

interface PresetDef {
  baseUrl: string;
  model: string;
  anthropicUrl?: string;
}

const PROVIDER_PRESETS: Record<string, PresetDef> = {
  'DeepSeek': {
    baseUrl: 'https://api.deepseek.com',
    anthropicUrl: 'https://api.deepseek.com/anthropic',
    model: 'deepseek-v4-flash, deepseek-v4-pro',
  },
};


// ── Top-level — switches on settingsTab ──────────────────────────────────

export default function SettingsView({ settingsTab }: { settingsTab: SettingsTab }) {
  if (settingsTab === 'llm') return <LlmConfig />;
  if (settingsTab === 'routing') return <RoutingConfig />;
  return null;
}

// ═══════════════════════════════════════════════════════════════════════════
// LLM Config Tab
// ═══════════════════════════════════════════════════════════════════════════

function LlmConfig() {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [loading, setLoading] = useState(true);
  // Draft: the provider currently being added or edited (null = form hidden)
  const [draft, setDraft] = useState<Provider | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null); // null = adding new, string = editing existing
  const [draftPreset, setDraftPreset] = useState('DeepSeek');
  // Test-connection for the draft
  const [testState, setTestState] = useState<'idle' | 'testing' | 'ok' | 'error'>('idle');
  const [testMsg, setTestMsg] = useState('');
  const [showDraftKey, setShowDraftKey] = useState(false);
  // Balances per provider — cached with 5-minute TTL
  const [balances, setBalances] = useState<Record<string, any>>({});
  const [balanceAge, setBalanceAge] = useState<string>('');
  const CACHE_KEY = 'everevo_balance_cache';
  const CACHE_TTL = 5 * 60 * 1000; // 5 minutes
  const presetKeys = Object.keys(PROVIDER_PRESETS);

  const loadCachedBalances = () => {
    try {
      const raw = localStorage.getItem(CACHE_KEY);
      if (raw) {
        const cached = JSON.parse(raw);
        if (Date.now() - cached.ts < CACHE_TTL) {
          setBalances(cached.data);
          setBalanceAge(fmtAge(cached.ts));
          return true;
        }
      }
    } catch { /* ignore */ }
    return false;
  };

  const saveCachedBalances = (data: Record<string, any>) => {
    const ts = Date.now();
    localStorage.setItem(CACHE_KEY, JSON.stringify({ ts, data }));
    setBalanceAge(fmtAge(ts));
  };

  // Fetch balances — respects cache unless forced
  const fetchBalances = async (force = false) => {
    if (!force && loadCachedBalances()) return;
    try {
      const res = await fetch('/api/balance');
      const json = await res.json();
      const map: Record<string, any> = {};
      for (const b of json.balances ?? []) {
        map[b.provider_id] = b;
      }
      setBalances(map);
      saveCachedBalances(map);
    } catch { /* ignore */ }
  };

  useEffect(() => { fetchBalances(); }, []);
  const getPreset = (name: string) => (PROVIDER_PRESETS as Record<string, PresetDef>)[name];

  // Resolve the correct base URL for a preset given current API format
  const resolveBaseUrl = (preset: PresetDef | undefined, format: string) => {
    if (!preset) return '';
    if (format === 'anthropic' && preset.anthropicUrl) return preset.anthropicUrl;
    return preset.baseUrl;
  };

  useEffect(() => {
    fetch('/api/config').then(r => r.json()).then(data => {
      if (data.llm && data.llm.length > 0) {
        setProviders(data.llm.map((p: any) => ({
          id: p.id || crypto.randomUUID(),
          api_format: p.api_format || 'openai',
          api_key: p.api_key || '',
          base_url: p.base_url || '',
          model: p.model || '',
        })));
      }
      setLoading(false);
    });
  }, []);

  // ── Draft helpers ─────────────────────────────────────────────────

  const openAddForm = () => {
    const id = crypto.randomUUID();
    const first = presetKeys[0];
    const p = getPreset(first);
    setDraft({ id, api_format: 'openai', api_key: '', base_url: p?.baseUrl || '', model: '' });
    setDraftPreset(first || '自定义');
    setEditingId(null);
    setTestState('idle'); setTestMsg('');
    setShowDraftKey(false);
  };

  const openEditForm = (p: Provider) => {
    setDraft({ ...p });
    setEditingId(p.id);
    const match = presetKeys.find(k => {
      const def = getPreset(k);
      return def && (def.baseUrl === p.base_url || def.anthropicUrl === p.base_url);
    });
    setDraftPreset(match || '自定义');
    setTestState('idle'); setTestMsg('');
  };

  const closeDraft = () => { setDraft(null); setEditingId(null); setTestState('idle'); setTestMsg(''); setShowDraftKey(false); };

  const updateDraft = (field: string, value: string) => {
    setDraft(prev => prev ? { ...prev, [field]: value } : null);
  };

  // Switch API format — also adjust base URL if the preset has a format-specific URL
  const setApiFormat = (format: string) => {
    const def = getPreset(draftPreset);
    const url = resolveBaseUrl(def, format);
    setDraft(prev => prev ? { ...prev, api_format: format, base_url: url } : null);
  };

  const applyDraftPreset = (name: string) => {
    setDraftPreset(name);
    const def = getPreset(name);
    if (def) {
      const format = draft?.api_format || 'openai';
      updateDraft('base_url', resolveBaseUrl(def, format));
    }
  };

  // ── Test connection for the draft ──────────────────────────────────

  const testDraft = async () => {
    if (!draft) return;
    if (!draft.api_key) {
      setTestState('error'); setTestMsg('请先输入 API Key'); return;
    }
    setTestState('testing'); setTestMsg('测试中...');

    // Build payload: put the draft first so backend verify hits it
    const all = [draft, ...providers.filter(p => editingId === null || p.id !== editingId)];
    try {
      await fetch('/api/config', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ llm: all }) });
      await fetch('/api/config/reload');
      const res = await fetch('/api/config/verify');
      const data = await res.json();
      if (data.ok) {
        setTestState('ok'); setTestMsg('已连接 ✓');
      } else {
        setTestState('error'); setTestMsg(data.error || '验证失败');
      }
    } catch {
      setTestState('error'); setTestMsg('网络错误');
    }
  };

  // ── Commit draft to list ───────────────────────────────────────────

  const commitDraft = () => {
    if (!draft) return;
    if (editingId) {
      setProviders(prev => prev.map(p => p.id === editingId ? draft : p));
    } else {
      setProviders(prev => [...prev, draft]);
    }
    closeDraft();
  };

  // ── Delete (auto-saves to backend) ────────────────────────────────

  const deleteProvider = async (id: string) => {
    const remaining = providers.filter(p => p.id !== id);
    setProviders(remaining);
    if (editingId === id) closeDraft();
    // Persist removal
    await fetch('/api/config', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ llm: remaining }) });
    await fetch('/api/config/reload');
  };

  if (loading) return (
    <div className="flex items-center justify-center flex-1 bg-background">
      <div className="animate-spin w-8 h-8 border-4 border-primary border-t-transparent rounded-full" />
    </div>
  );

  return (
    <div className="flex flex-col flex-1 min-h-0 max-w-xl mx-auto p-4 gap-4 bg-background w-full">
      <main className="flex-1 space-y-3 overflow-y-auto">
        {/* ── Draft form ─────────────────────────────────────────── */}
        {draft && (
          <div className="bg-secondary/50 border border-warning/30 rounded-xl p-4 space-y-3 animate-scale-in">
            <div className="flex items-center justify-between">
              <span className="text-sm font-semibold text-foreground">
                {editingId ? '编辑模型' : '添加模型'}
              </span>
              <button onClick={closeDraft} className="text-muted-foreground hover:text-foreground text-sm">收起</button>
            </div>

            {/* API format */}
            <div className="flex gap-2">
              <button onClick={() => setApiFormat('openai')}
                className={`flex-1 py-1.5 rounded text-xs transition-colors ${
                  draft.api_format === 'openai' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-muted-foreground hover:text-foreground'
                }`}>OpenAI 兼容</button>
              <button onClick={() => setApiFormat('anthropic')}
                className={`flex-1 py-1.5 rounded text-xs transition-colors ${
                  draft.api_format === 'anthropic' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-muted-foreground hover:text-foreground'
                }`}>Anthropic</button>
            </div>

            {/* Provider preset */}
            <div>
              <label className="text-xs text-muted-foreground">Provider</label>
              <div className="flex gap-1 mt-0.5 flex-wrap">
                {presetKeys.map(name => (
                  <button key={name} onClick={() => applyDraftPreset(name)}
                    className={`text-xs px-2 py-1 rounded transition-colors ${
                      draftPreset === name ? 'bg-primary text-primary-foreground' : 'bg-secondary text-muted-foreground hover:text-foreground'
                    }`}>{name}</button>
                ))}
                <button onClick={() => applyDraftPreset('自定义')}
                  className={`text-xs px-2 py-1 rounded transition-colors ${
                    draftPreset === '自定义' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-muted-foreground hover:text-foreground'
                  }`}>自定义</button>
              </div>
            </div>

            {/* API Key */}
            <div>
              <label className="text-xs text-muted-foreground">API Key</label>
              <div className="flex gap-1 mt-0.5">
                <div className="relative flex-1">
                  <input type={showDraftKey ? 'text' : 'password'} value={draft.api_key} onChange={e => updateDraft('api_key', e.target.value)}
                    placeholder={draft.api_format === 'anthropic' ? 'sk-ant...' : 'sk-...'}
                    className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs font-mono pr-12
                               text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
                  <button onClick={() => setShowDraftKey(!showDraftKey)}
                    className="absolute right-1.5 top-1/2 -translate-y-1/2 text-xs text-muted-foreground hover:text-foreground
                               bg-secondary px-1.5 py-0.5 rounded transition-colors">
                    {showDraftKey ? '隐藏' : '显示'}
                  </button>
                </div>
              </div>
            </div>

            {/* Base URL */}
            <div>
              <label className="text-xs text-muted-foreground">Base URL</label>
              <input type="text" value={draft.base_url} onChange={e => updateDraft('base_url', e.target.value)}
                className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs font-mono mt-0.5
                           text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
            </div>

            {/* Model */}
            <div>
              <label className="text-xs text-muted-foreground">Model</label>
              <input type="text" value={draft.model} onChange={e => updateDraft('model', e.target.value)}
                placeholder={getPreset(draftPreset)?.model || 'model-name'}
                className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs font-mono mt-0.5
                           text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
            </div>

            {/* Test + Commit buttons */}
            <div className="flex gap-2 pt-1">
              <button onClick={testDraft} disabled={testState === 'testing'}
                className={`flex-1 py-2 rounded-lg text-xs font-medium border transition-colors ${
                  testState === 'testing' ? 'border-warning/50 text-warning bg-warning/5' :
                  testState === 'ok' ? 'border-success/50 text-success bg-success/5' :
                  testState === 'error' ? 'border-destructive/50 text-destructive bg-destructive/5' :
                  'border-border text-muted-foreground hover:text-foreground'
                }`}>
                {testState === 'testing' ? '⏳ 测试中...' : testState === 'ok' ? '✓ 已连接' : testState === 'error' ? `✗ ${testMsg}` : '🔍 测试连接'}
              </button>
              <button onClick={commitDraft}
                disabled={!draft.model || testState !== 'ok'}
                className={`py-2 px-4 rounded-lg text-xs font-medium transition-colors ${
                  !draft.model || testState !== 'ok'
                    ? 'bg-muted text-muted-foreground cursor-not-allowed'
                    : 'bg-primary hover:bg-primary/90 text-primary-foreground'
                }`}>
                {editingId ? '保存更改' : '添加到列表'}
              </button>
            </div>
          </div>
        )}

        {/* ── Add + Refresh row ─────────────────────────────────── */}
        {!draft && (
          <div className="flex gap-2">
            <button onClick={openAddForm}
              className="flex-[3] py-2.5 rounded-lg text-xs font-medium border-2 border-dashed border-border text-muted-foreground
                         hover:border-primary hover:text-primary transition-colors">
              + 添加模型
            </button>
            <button onClick={() => fetchBalances(true)}
              className="flex-1 py-2.5 rounded-lg text-xs font-medium border-2 border-dashed border-border text-muted-foreground
                         hover:border-primary hover:text-primary transition-colors">
              刷新余额{balanceAge ? ` · ${balanceAge}` : ''}
            </button>
          </div>
        )}

        {/* ── Provider list ──────────────────────────────────────── */}
        {providers.length === 0 && !draft && (
          <p className="text-xs text-muted-foreground text-center py-4">暂无模型</p>
        )}
        {providers.map((p, i) => (
          <div key={p.id}
            onClick={() => editingId !== p.id && openEditForm(p)}
            className={`bg-secondary/50 border rounded-xl p-3 cursor-pointer transition-all duration-300 border-l-[3px] hover:bg-secondary`}
            style={{ borderLeftColor: editingId === p.id ? 'var(--warning)' : 'transparent' }}
          >
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2 min-w-0">
                <span className="text-xs text-muted-foreground shrink-0">{i + 1}.</span>
                <span className="text-xs font-medium text-foreground truncate font-mono">{p.model || '(未命名)'}</span>
                <span className={`text-xs px-1.5 py-0.5 rounded shrink-0 ${
                  p.api_format === 'anthropic' ? 'bg-accent text-accent-foreground' : 'bg-primary/20 text-primary-foreground'
                }`}>{p.api_format === 'anthropic' ? 'Anthropic' : 'OpenAI'}</span>
              </div>
              <button onClick={(e) => { e.stopPropagation(); deleteProvider(p.id); }}
                className="text-muted-foreground hover:text-destructive text-sm shrink-0 ml-2">×</button>
            </div>
            <p className="text-xs text-muted-foreground truncate mt-1">{p.base_url}</p>
            {/* Per-provider balance — stable height placeholder */}
            <div className="mt-1.5 pt-1.5 border-t border-border/50 min-h-[1.25rem]">
              {!balances[p.id] ? (
                <span className="text-xs text-muted-foreground/40">查询中...</span>
              ) : balances[p.id].ok && balances[p.id].data?.balance_infos?.map((bi: any, j: number) => (
                <div key={j} className="flex items-center gap-2">
                  <span className={`inline-block w-2 h-2 rounded-full shrink-0 ${balances[p.id].data?.is_available !== false ? 'bg-success' : 'bg-destructive'}`} />
                  <span className="text-xs font-mono text-foreground">{bi.total_balance} {bi.currency}</span>
                  <span className="text-xs text-muted-foreground">
                    (充值 {bi.topped_up_balance} + 赠送 {bi.granted_balance})
                  </span>
                </div>
              ))}
              {balances[p.id] && !balances[p.id].ok && (
                <p className="text-xs text-destructive">{balances[p.id].error}</p>
              )}
            </div>
          </div>
        ))}

      </main>

    </div>
  );
}

// ── Helpers ────────────────────────────────────────────────────────────

function fmtAge(ts: number): string {
  const sec = Math.floor((Date.now() - ts) / 1000);
  if (sec < 60) return '刚刚';
  if (sec < 3600) return `${Math.floor(sec / 60)} 分钟前`;
  if (sec < 86400) return `${Math.floor(sec / 3600)} 小时前`;
  return `${Math.floor(sec / 86400)} 天前`;
}

// ═══════════════════════════════════════════════════════════════════════════
// Routing Config — first-try-cheap escalation cascade
//
// BitRouter, ModelCascade, CRC Router all converge: start cheapest, escalate on failure.
// Agent (LLM) decides when — no keyword matcher, no pre-classifier.
// ═══════════════════════════════════════════════════════════════════════════

const EFFORT_OPTIONS: { value: EffortLevel; label: string }[] = [
  { value: 'auto', label: '自动' },
  { value: 'off',  label: '关闭' },
  { value: 'high', label: '高' },
  { value: 'max',  label: '最大' },
];

const CASCADE_LABELS = [
  { label: 'L1 · 默认执行' },
  { label: 'L2 · 升级执行' },
  { label: 'L3 · 强力执行' },
];

function RoutingConfig() {
  const { config, setMainModel, setMainEffort, setTier } = useRoutingConfig();
  const [providers, setProviders] = useState<Provider[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetch('/api/config').then(r => r.json()).then(data => {
      const list: Provider[] = (data.llm ?? []).map((p: any) => ({
        id: p.id || crypto.randomUUID(),
        api_format: p.api_format || 'openai', api_key: '',
        base_url: p.base_url || '', model: p.model || '',
      }));
      setProviders(list);
      if (list.length > 0) {
        const first = list[0]?.id ?? '';
        const second = list.length >= 2 ? (list[1]?.id ?? first) : first;
        if (!config.mainModelId) setMainModel(first);
        if (!config.tiers[0].modelId) {
          setTier(0, 'modelId', second);
          setTier(1, 'modelId', first);
          setTier(2, 'modelId', first);
        }
      }
      setLoading(false);
    }).catch(() => setLoading(false));
  }, []);

  if (loading) return (
    <div className="flex items-center justify-center flex-1 bg-background">
      <div className="animate-spin w-8 h-8 border-4 border-primary border-t-transparent rounded-full" />
    </div>
  );

  return (
    <div className="flex flex-col flex-1 min-h-0 max-w-xl mx-auto p-4 gap-4 bg-background w-full">
      <main className="flex-1 space-y-4 overflow-y-auto">
        {providers.length < 2 && (
          <div className="bg-warning/10 border border-warning/30 rounded-xl p-3 text-xs text-warning">
            ⚠️ 需要至少两个模型才能启用级联。请先在「大语言模型」中添加。
          </div>
        )}

        {/* Main agent */}
        <div>
          <p className="text-xs text-muted-foreground font-medium mb-2">主 Agent（编排者）</p>
          <p className="text-xs text-muted-foreground mb-2">
            负责规划、决策、判断。不参与级联。
          </p>
          <div className="bg-secondary/50 border border-border rounded-xl p-3 flex gap-2 items-center">
            <span className="text-xs text-foreground shrink-0 w-12">编排者</span>
            <select value={config.mainModelId}
              onChange={e => setMainModel(e.target.value)}
              className="flex-1 bg-background border border-border rounded px-2.5 py-1.5 text-xs text-foreground
                         focus:outline-none focus:border-primary appearance-none cursor-pointer">
              {providers.map((p) => (
                <option key={p.id} value={p.id}>{p.model || '(未命名)'}</option>
              ))}
            </select>
            <select value={config.mainEffort}
              onChange={e => setMainEffort(e.target.value as EffortLevel)}
              className="w-20 bg-background border border-border rounded px-2 py-1.5 text-xs text-foreground
                         focus:outline-none focus:border-primary appearance-none cursor-pointer">
              {EFFORT_OPTIONS.map(o => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </div>
        </div>

        {/* Cascade tiers */}
        <div>
          <p className="text-xs text-muted-foreground font-medium mb-2">子 Agent 执行链路</p>
          <p className="text-xs text-muted-foreground mb-2">
            仅影响子 Agent，不预分类不硬编码。
          </p>
          <div className="space-y-1.5">
            {config.tiers.map((tier, i) => (
              <div key={i}
                className="bg-secondary/50 border border-border rounded-xl p-3 flex gap-2 items-center transition-all duration-300"
              >
                <span className="text-xs text-muted-foreground shrink-0 w-12">{CASCADE_LABELS[i].label.split('·')[0].trim()}</span>
                <select value={tier.modelId}
                  onChange={e => setTier(i, 'modelId', e.target.value)}
                  className="flex-1 bg-background border border-border rounded px-2.5 py-1.5 text-xs text-foreground
                             focus:outline-none focus:border-primary appearance-none cursor-pointer">
                  {providers.map((p) => (
                    <option key={p.id} value={p.id}>{p.model || '(未命名)'}</option>
                  ))}
                </select>
                <select value={tier.effort}
                  onChange={e => setTier(i, 'effort', e.target.value as EffortLevel)}
                  className="w-20 bg-background border border-border rounded px-2 py-1.5 text-xs text-foreground
                             focus:outline-none focus:border-primary appearance-none cursor-pointer">
                  {EFFORT_OPTIONS.map(o => (
                    <option key={o.value} value={o.value}>{o.label}</option>
                  ))}
                </select>
              </div>
            ))}
          </div>
        </div>

        <div className="bg-secondary/30 border border-border rounded-xl p-3">
          <p className="text-xs text-muted-foreground leading-relaxed">
            主 Agent（编排者）始终用当前对话模型，保证编排质量。
            子 Agent（执行者）按此链路升级——先试便宜，失败升级。
            参考：Cursor Swarm（planner 用最强，worker 级联）、
            Claude Code（主对话 Sonnet/Opus，子 Agent Haiku/Sonnet）。
          </p>
        </div>
      </main>
    </div>
  );
}
