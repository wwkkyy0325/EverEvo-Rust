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

  if (loading) return <div className="flex items-center justify-center h-screen"><div className="animate-spin w-8 h-8 border-4 border-blue-500 border-t-transparent rounded-full" /></div>;

  const ProviderCard = ({ p, idx, label, desc }: { p: Provider; idx: number; label: string; desc: string }) => {
    const [showKey, setShowKey] = useState(false);
    return (
      <div className="bg-gray-900/50 border border-gray-700 rounded-xl p-4 space-y-3">
        <div className="flex items-center justify-between">
          <span className="text-sm font-semibold">{label}</span>
          <span className="text-xs text-gray-500">{desc}</span>
        </div>
        <div className="flex gap-2">
          <button onClick={() => update(idx, 'api_format', 'openai')} className={`flex-1 py-1.5 rounded text-xs border ${p.api_format === 'openai' ? 'border-blue-500 bg-blue-900/30 text-blue-300' : 'border-gray-600 text-gray-500'}`}>OpenAI 兼容</button>
          <button onClick={() => update(idx, 'api_format', 'anthropic')} className={`flex-1 py-1.5 rounded text-xs border ${p.api_format === 'anthropic' ? 'border-blue-500 bg-blue-900/30 text-blue-300' : 'border-gray-600 text-gray-500'}`}>Anthropic</button>
        </div>
        <div>
          <label className="text-xs text-gray-500">API Key</label>
          <div className="relative mt-0.5">
            <input type={showKey ? 'text' : 'password'} value={p.api_key} onChange={e => update(idx, 'api_key', e.target.value)}
              placeholder={p.api_format === 'anthropic' ? 'sk-ant...' : 'sk-...'}
              className="w-full bg-gray-800 border border-gray-600 rounded px-2.5 py-1.5 text-xs font-mono pr-14" />
            <button onClick={() => setShowKey(!showKey)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-xs text-gray-500 hover:text-gray-300 bg-gray-800 px-1.5 py-0.5 rounded">
              {showKey ? '隐藏' : '显示'}
            </button>
          </div>
        </div>
        <div>
          <label className="text-xs text-gray-500">Base URL</label>
          <input type="text" value={p.base_url} onChange={e => update(idx, 'base_url', e.target.value)}
            className="w-full bg-gray-800 border border-gray-600 rounded px-2.5 py-1.5 text-xs font-mono mt-0.5" />
        </div>
        <div>
          <label className="text-xs text-gray-500">Model</label>
          <input type="text" value={p.model} onChange={e => update(idx, 'model', e.target.value)}
            placeholder="deepseek-chat"
            className="w-full bg-gray-800 border border-gray-600 rounded px-2.5 py-1.5 text-xs mt-0.5" />
        </div>
      </div>
    );
  };

  return (
    <div className="flex flex-col h-screen max-w-xl mx-auto p-4 gap-4">
      <header className="flex items-center gap-3 pt-6">
        <button onClick={onBack} className="text-gray-400 hover:text-white text-sm">← 返回</button>
        <h1 className="text-lg font-bold">模型配置</h1>
      </header>

      <main className="flex-1 space-y-4 overflow-y-auto">
        <ProviderCard p={providers[0]} idx={0} label="🥇 主力模型" desc="默认使用" />
        <ProviderCard p={providers[1]} idx={1} label="🥈 辅助模型" desc="备用 / 对比" />

        <p className="text-xs text-gray-600">
          配置保存到 <code className="bg-gray-800 px-1 rounded">data/config.toml</code>，明文存储。
        </p>
      </main>

      {status !== 'idle' && (
        <div className={`text-xs p-2.5 rounded-lg text-center border ${
          status === 'ok' ? 'bg-green-900/30 text-green-300 border-green-800' :
          status === 'error' ? 'bg-red-900/30 text-red-300 border-red-800' :
          'bg-blue-900/30 text-blue-300 border-blue-800'
        }`}>
          {['saving','verifying','reloading'].includes(status) ? <span className="inline-flex items-center gap-1.5"><span className="animate-spin w-3 h-3 border-2 border-current border-t-transparent rounded-full" />{statusMsg}</span> : statusMsg}
        </div>
      )}

      <footer className="pb-4 flex gap-2">
        <button onClick={saveAndVerify} disabled={['saving','verifying','reloading'].includes(status)}
          className={`flex-1 py-2.5 rounded-lg text-sm font-medium transition-colors ${
            ['saving','verifying','reloading'].includes(status) ? 'bg-gray-600 cursor-not-allowed' :
            status === 'ok' ? 'bg-green-600' : 'bg-blue-600 hover:bg-blue-500'
          }`}>
          {status === 'saving' ? '保存中...' : status === 'reloading' ? '加载中...' : status === 'verifying' ? '验证中...' : status === 'ok' ? '已生效 ✓' : '保存并验证'}
        </button>
        <button onClick={onBack} className="px-3 py-2.5 rounded-lg text-sm bg-gray-700 hover:bg-gray-600">取消</button>
      </footer>
    </div>
  );
}
