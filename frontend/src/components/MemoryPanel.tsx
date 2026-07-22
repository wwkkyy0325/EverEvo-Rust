// Memory Panel — browse and manage persistent facts from data/memory/facts/

import { useState, useEffect } from 'react';
import { useStore } from '../store';

interface FactItem {
  name: string;
  description: string;
  fact_type: string;
  created_at: string;
  updated_at: string;
}

export default function MemoryPanel() {
  const { showMemory, toggleMemory } = useStore();
  const [facts, setFacts] = useState<FactItem[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (showMemory) loadFacts();
  }, [showMemory]);

  const loadFacts = async () => {
    setLoading(true);
    try {
      const res = await fetch('/api/memory/facts');
      const json = await res.json();
      setFacts(json.data?.facts ?? []);
    } catch { /* ignore */ }
    setLoading(false);
  };

  const deleteFact = async (name: string) => {
    await fetch(`/api/memory/facts/${encodeURIComponent(name)}`, { method: 'DELETE' });
    loadFacts();
  };

  if (!showMemory) return null;

  return (
    <div className="fixed right-0 top-0 h-screen w-80 bg-background border-l border-border z-40 overflow-y-auto shadow-2xl">
      <div className="p-4">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-bold text-foreground">🧠 Memory ({facts.length})</h2>
          <button onClick={toggleMemory} className="text-muted-foreground hover:text-foreground text-lg">&times;</button>
        </div>

        <button onClick={loadFacts} className="text-xs text-thinking hover:text-thinking/80 mb-3">🔄 Refresh</button>

        {loading && <p className="text-xs text-muted-foreground text-center py-4">Loading...</p>}
        {!loading && facts.length === 0 && (
          <p className="text-xs text-muted-foreground text-center py-4">
            No facts yet. Ask the assistant to remember something.
          </p>
        )}

        <div className="space-y-2">
          {facts.map((f) => (
            <div key={f.name} className="bg-secondary rounded p-2.5 border border-border/50 group">
              <div className="flex items-center gap-2 mb-1">
                <span className={`text-[10px] px-1.5 py-0.5 rounded ${
                  f.fact_type === 'user' ? 'bg-primary/30 text-primary-foreground' :
                  f.fact_type === 'feedback' ? 'bg-warning/20 text-warning' :
                  f.fact_type === 'reference' ? 'bg-success/20 text-success' :
                  'bg-muted text-muted-foreground'
                }`}>{f.fact_type}</span>
                <span className="text-xs font-medium text-foreground truncate flex-1">{f.name}</span>
                <button onClick={() => deleteFact(f.name)}
                  className="text-muted-foreground hover:text-destructive text-xs opacity-0 group-hover:opacity-100 transition-opacity">
                  🗑
                </button>
              </div>
              <p className="text-xs text-muted-foreground truncate">{f.description}</p>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
