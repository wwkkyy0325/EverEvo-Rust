import { useState, useEffect } from 'react';
import { useStore } from '../store';

interface FactItem {
  name: string; description: string; fact_type: string;
  created_at: string; updated_at: string;
}

export default function MemoryPanel() {
  const showMemory = useStore((s) => s.showMemory);
  const [facts, setFacts] = useState<FactItem[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => { if (showMemory) loadFacts(); }, [showMemory]);

  const loadFacts = async () => {
    setLoading(true);
    try { const r = await fetch('/api/memory/facts'); const j = await r.json(); setFacts(j.data?.facts ?? []); } catch { /* */ }
    setLoading(false);
  };
  const deleteFact = async (name: string) => {
    await fetch(`/api/memory/facts/${encodeURIComponent(name)}`, { method: 'DELETE' });
    loadFacts();
  };
  const toggle = () => useStore.setState((s) => ({ showMemory: !s.showMemory }));

  if (!showMemory) return null;
  return (
    <div className="fixed right-0 top-0 h-screen w-80 bg-background border-l border-border z-40 overflow-y-auto shadow-2xl">
      <div className="p-4">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-bold">🧠 Memory ({facts.length})</h2>
          <button onClick={toggle} className="text-muted-foreground hover:text-foreground text-lg">&times;</button>
        </div>
        <button onClick={loadFacts} className="text-xs text-primary hover:underline mb-3">🔄 Refresh</button>
        {loading && <p className="text-xs text-muted-foreground text-center py-4">Loading...</p>}
        {!loading && facts.length === 0 && <p className="text-xs text-muted-foreground text-center py-4">No facts yet.</p>}
        <div className="space-y-2">
          {facts.map((f) => (
            <div key={f.name} className="bg-secondary rounded p-2.5 border border-border/50 group">
              <div className="flex items-center gap-2 mb-1">
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-primary/20">{f.fact_type}</span>
                <span className="text-xs font-medium truncate flex-1">{f.name}</span>
                <button onClick={() => deleteFact(f.name)} className="text-muted-foreground hover:text-destructive text-xs opacity-0 group-hover:opacity-100">🗑</button>
              </div>
              <p className="text-xs text-muted-foreground truncate">{f.description}</p>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
