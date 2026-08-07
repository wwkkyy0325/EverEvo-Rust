import { useEffect, useState } from 'react';

// ── Types ───────────────────────────────────────────────────────────────

interface Character {
  name: string;
  identity: string;
  traits: string[];
  tone: string;
  style_guidelines: string[];
  values: string[];
  voice_samples: string;
}

// split helpers — comma for short lists, newline for long guidelines
const toList = (text: string) =>
  text.split('\n').flatMap(l => l.split(',')).map(s => s.trim()).filter(Boolean);
const fromList = (arr: string[]) => arr.join(', ');

// ── Component ───────────────────────────────────────────────────────────

export default function CharacterConfig() {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null);

  // Form fields (arrays edited as text)
  const [name, setName] = useState('');
  const [identity, setIdentity] = useState('');
  const [tone, setTone] = useState('');
  const [traitsText, setTraitsText] = useState('');
  const [styleText, setStyleText] = useState('');
  const [valuesText, setValuesText] = useState('');
  const [voiceSamples, setVoiceSamples] = useState('');

  useEffect(() => {
    fetch('/api/character')
      .then(r => r.json())
      .then((c: Character) => {
        setName(c.name ?? '');
        setIdentity(c.identity ?? '');
        setTone(c.tone ?? '');
        setTraitsText(fromList(c.traits ?? []));
        setStyleText((c.style_guidelines ?? []).join('\n'));
        setValuesText(fromList(c.values ?? []));
        setVoiceSamples(c.voice_samples ?? '');
      })
      .catch(() => setMsg({ kind: 'err', text: '加载人格失败' }))
      .finally(() => setLoading(false));
  }, []);

  const save = async () => {
    setSaving(true);
    setMsg(null);
    const payload: Character = {
      name: name.trim(),
      identity: identity.trim(),
      traits: toList(traitsText),
      tone: tone.trim(),
      style_guidelines: toList(styleText),
      values: toList(valuesText),
      voice_samples: voiceSamples,
    };
    if (!payload.name) {
      setMsg({ kind: 'err', text: '名字不能为空' });
      setSaving(false);
      return;
    }
    try {
      const res = await fetch('/api/character', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `HTTP ${res.status}`);
      }
      setMsg({ kind: 'ok', text: '已保存 ✓ 下一轮对话生效' });
    } catch (e: any) {
      setMsg({ kind: 'err', text: `保存失败：${e.message || e}` });
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center flex-1 bg-background">
        <div className="animate-spin w-8 h-8 border-4 border-primary border-t-transparent rounded-full" />
      </div>
    );
  }

  return (
    <div className="flex flex-col flex-1 min-h-0 max-w-xl mx-auto p-4 gap-4 bg-background w-full">
      <main className="flex-1 space-y-3 overflow-y-auto">
        <div className="flex items-center justify-between">
          <h2 className="text-sm font-semibold text-foreground">🎭 人格声音</h2>
          <span className="text-xs text-muted-foreground">定义 EverEvo 的说话风格</span>
        </div>

        {/* ── Identity ─────────────────────────────────────────── */}
        <div className="bg-secondary/50 border border-border rounded-xl p-4 space-y-3">
          <div>
            <label className="text-xs text-muted-foreground">名字</label>
            <input type="text" value={name} onChange={e => setName(e.target.value)}
              className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5
                         text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">身份（一句话：是谁/做什么）</label>
            <input type="text" value={identity} onChange={e => setIdentity(e.target.value)}
              placeholder="a desktop AI coding companion"
              className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5
                         text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">语气</label>
            <input type="text" value={tone} onChange={e => setTone(e.target.value)}
              placeholder="concise, direct, expert peer"
              className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5
                         text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">特质（逗号分隔）</label>
            <input type="text" value={traitsText} onChange={e => setTraitsText(e.target.value)}
              placeholder="curious, honest, pragmatic"
              className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5
                         text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
          </div>
          <div>
            <label className="text-xs text-muted-foreground">价值观（逗号分隔）</label>
            <input type="text" value={valuesText} onChange={e => setValuesText(e.target.value)}
              placeholder="correctness over speed, simplicity over cleverness"
              className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5
                         text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
          </div>
        </div>

        {/* ── Speaking style ───────────────────────────────────── */}
        <div className="bg-secondary/50 border border-border rounded-xl p-4 space-y-2">
          <label className="text-xs text-muted-foreground">说话规则（每行一条）</label>
          <textarea value={styleText} onChange={e => setStyleText(e.target.value)} rows={5}
            placeholder={"Lead with the answer or code; explain after.\nPush back on overcomplicated approaches."}
            className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5 font-mono
                       text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary resize-y" />
        </div>

        {/* ── Voice samples / fragments ───────────────────────── */}
        <div className="bg-secondary/50 border border-border rounded-xl p-4 space-y-2">
          <label className="text-xs text-muted-foreground">
            声音样本 / 碎片（粘贴聊天记录、文献摘录、风格笔记——原文注入）
          </label>
          <textarea value={voiceSamples} onChange={e => setVoiceSamples(e.target.value)} rows={5}
            placeholder={"User: 帮我加缓存\nAgent: moka 即可，别上 Redis——单机桌面过度设计。"}
            className="w-full bg-background border border-border rounded px-2.5 py-1.5 text-xs mt-0.5 font-mono
                       text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-primary resize-y" />
          <p className="text-xs text-muted-foreground leading-relaxed">
            也可把 <code className="px-1 py-0.5 bg-background rounded">.md</code> / <code className="px-1 py-0.5 bg-background rounded">.txt</code> 碎片文件放进{' '}
            <code className="px-1 py-0.5 bg-background rounded">data/memory/agent/sources/</code>，自动加载。
            想让大模型从这些碎片自动提炼成上面的结构化字段？在聊天框输入{' '}
            <code className="px-1 py-0.5 bg-background rounded">/character sync</code>。
          </p>
        </div>

        {msg && (
          <div className={`text-xs px-3 py-2 rounded-lg ${
            msg.kind === 'ok' ? 'bg-primary/10 text-primary' : 'bg-destructive/10 text-destructive'
          }`}>
            {msg.text}
          </div>
        )}
      </main>

      {/* ── Save bar ─────────────────────────────────────────── */}
      <footer className="shrink-0 flex justify-end">
        <button onClick={save} disabled={saving}
          className="px-4 py-2 rounded-lg text-xs font-medium bg-primary hover:bg-primary/90 text-primary-foreground
                     transition-colors disabled:opacity-50 disabled:cursor-not-allowed">
          {saving ? '保存中…' : '保存'}
        </button>
      </footer>
    </div>
  );
}
