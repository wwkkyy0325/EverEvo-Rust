import { useState, useEffect } from 'react';

interface Provider {
  id: string;
  api_format: string;
  api_key: string;
  base_url: string;
  model: string;
}

const DEFAULT_PRIMARY: Provider = { id: 'primary', api_format: 'openai', api_key: '', base_url: 'https://api.deepseek.com/v1', model: 'deepseek-chat' };
const DEFAULT_SECONDARY: Provider = { id: 'secondary', api_format: 'anthropic', api_key: '', base_url: 'https://api.anthropic.com', model: 'claude-haiku-4-5-20251001' };

export default function SettingsView({ onBack }: { onBack: () => void }) {
  const [providers, setProviders] = useState<Provider[]>([{ ...DEFAULT_PRIMARY }, { ...DEFAULT_SECONDARY }]);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState<'idle'|'saving'|'verifying'|'reloading'|'ok'|'error'>('idle');
  const [statusMsg, setStatusMsg] = useState('');

  useEffect(() => {
    fetch('/api/config').then(r => r.json()).then(data => {
      if (data.llm && data.llm.length > 0) setProviders(data.llm.map((p: any, i: number) => ({ ...DEFAULT_PRIMARY, ...p, id: p.id || (i === 0 ? 'primary' : 'secondary'), api_key: '' })));
      setLoading(false);
    });
  }, []);

  const update = (idx: number, field: string, value: string) => {
    setProviders(prev => prev.map((p, i) => i === idx ? { ...p, [field]: value } : p));
  };

  const saveAndVerify = async () => {
    setStatus('saving'); setStatusMsg('保存...');
    await fetch('/api/config', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ llm: providers }) });

    setStatus('reloading'); setStatusMsg('加载...');
    await fetch('/api/config/reload');

    setStatus('verifying'); setStatusMsg('验证主力模型...');
    try {
      const res = await fetch('/api/config/verify'); const data = await res.json();
      setStatus(data.ok ? 'ok' : 'error');
      setStatusMsg(data.ok ? `✅ ${(data.response || '').slice(0, 60)}` : (data.error || '验证失败'));
      if (data.ok) setTimeout(() => onBack(), 1500);
    } catch { setStatus('error'); setStatusMsg('连接失败'); }
  };

  if (loading) return (
    <div className="flex items-center justify-center flex-1 bg-background">
      <div className="animate-spin w-8 h-8 border-4 border-primary border-t-transparent rounded-full" />
    </div>
  );

  const ProviderCard = ({ p, idx, label, desc }: { p: Provider; idx: number; label: string; desc: string }) => {
    const [showKey, setShowKey] = useState(false);
    return (
      <div className="bg-secondary/50 border border-border rounded-xl p-4 space-y-3">
        <div className="flex items-center justify-between">
          <span className="text-sm font-semibold text-foreground">{label}</span>
          <span className="text-xs text-muted-foreground">{desc}</span>
        </div>
        <div className="flex gap-2">
          <button onClick={() => update(idx, 'api_format', 'openai')}
            className={`h-screen py-1.5 rounded text-xs border transition-colors ${
              p.api_format === 'openai'
                ? 'border-primary bg-primary/20 text-primary-foreground'
                : 'border-border text-muted-foreground hover:text-foreground'
            }`}>OpenAI 兼容</button>
          <button onClick={() => update(idx, 'api_format', 'anthropic')}
            className={`h-screen py-1.5 rounded text-xs border transition-colors ${
              p.api_format === 'anthropic'
                ? 'border-primary bg-primary/20 text-primary-foreground'
                : 'border-border text-muted-foreground hover:text-foreground'
            }`}>Anthropic</button>
        </div>
        <div>
          <label className="text-xs text-muted-foreground">API Key</label>
          <div className="relative mt-0.5">
            <input type={showKey ? 'text' : 'password'} value={p.api_key} onChange={e => update(idx, 'api_key', e.target.value)}
              placeholder={p.api_format === 'anthropic' ? 'sk-ant...' : 'sk-...'}
              className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs font-mono pr-14
                         text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
            <button onClick={() => setShowKey(!showKey)}
              className="absolute right-1.5 top-1/2 -translate-y-1/2 text-xs text-muted-foreground hover:text-foreground
                         bg-secondary px-1.5 py-0.5 rounded transition-colors">
              {showKey ? '隐藏' : '显示'}
            </button>
          </div>
        </div>
        <div>
          <label className="text-xs text-muted-foreground">Base URL</label>
          <input type="text" value={p.base_url} onChange={e => update(idx, 'base_url', e.target.value)}
            className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs font-mono mt-0.5
                       text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
        </div>
        <div>
          <label className="text-xs text-muted-foreground">Model</label>
          <input type="text" value={p.model} onChange={e => update(idx, 'model', e.target.value)}
            placeholder="deepseek-chat"
            className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5
                       text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
        </div>
      </div>
    );
  };

  return (
    <div className="flex flex-col flex-1 min-h-0 max-w-xl mx-auto p-4 gap-4 bg-background w-full">
      <header className="flex items-center gap-3 pt-6">
        <button onClick={onBack} className="text-muted-foreground hover:text-foreground text-sm transition-colors">← 返回</button>
        <h1 className="text-lg font-bold text-foreground">模型配置</h1>
      </header>

      <main className="h-screen space-y-4 overflow-y-auto">
        <ProviderCard p={providers[0]} idx={0} label="🥇 主力模型" desc="默认使用" />
        <ProviderCard p={providers[1]} idx={1} label="🥈 辅助模型" desc="备用 / 对比" />

        <p className="text-xs text-muted-foreground">
          配置保存到 <code className="bg-secondary px-1 rounded">data/config.toml</code>，明文存储。
        </p>
      </main>

      {status !== 'idle' && (
        <div className={`text-xs p-2.5 rounded-lg text-center border ${
          status === 'ok' ? 'bg-success/15 text-success border-success/50' :
          status === 'error' ? 'bg-destructive/15 text-destructive border-destructive/50' :
          'bg-primary/20 text-primary-foreground border-primary'
        }`}>
          {['saving','verifying','reloading'].includes(status) ? (
            <span className="inline-flex items-center gap-1.5">
              <span className="animate-spin w-3 h-3 border-2 border-current border-t-transparent rounded-full" />
              {statusMsg}
            </span>
          ) : statusMsg}
        </div>
      )}

      <footer className="pb-4 flex gap-2">
        <button onClick={saveAndVerify} disabled={['saving','verifying','reloading'].includes(status)}
          className={`h-screen py-2.5 rounded-lg text-sm font-medium transition-colors ${
            ['saving','verifying','reloading'].includes(status)
              ? 'bg-muted text-muted-foreground cursor-not-allowed'
              : status === 'ok'
              ? 'bg-success hover:bg-success/90 text-success-foreground'
              : 'bg-primary hover:bg-primary/90 text-primary-foreground'
          }`}>
          {status === 'saving' ? '保存中...' : status === 'reloading' ? '加载中...' : status === 'verifying' ? '验证中...' : status === 'ok' ? '已生效 ✓' : '保存并验证'}
        </button>
        <button onClick={onBack} className="px-3 py-2.5 rounded-lg text-sm bg-secondary hover:bg-secondary/80 transition-colors">取消</button>
      </footer>
    </div>
  );
}
