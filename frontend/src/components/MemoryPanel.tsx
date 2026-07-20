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
    <div className="fixed right-0 top-0 h-screen w-80 bg-gray-900 border-l border-gray-700 z-40 overflow-y-auto shadow-2xl">
      <div className="p-4">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-bold text-gray-200">🧠 Memory ({facts.length})</h2>
          <button onClick={toggleMemory} className="text-gray-500 hover:text-white text-lg">&times;</button>
        </div>

        <button onClick={loadFacts} className="text-xs text-purple-400 hover:text-purple-300 mb-3">🔄 Refresh</button>

        {loading && <p className="text-xs text-gray-500 text-center py-4">Loading...</p>}
        {!loading && facts.length === 0 && (
          <p className="text-xs text-gray-600 text-center py-4">
            No facts yet. Ask the assistant to remember something.
          </p>
        )}

        <div className="space-y-2">
          {facts.map((f) => (
            <div key={f.name} className="bg-gray-800 rounded p-2.5 border border-gray-700/50 group">
              <div className="flex items-center gap-2 mb-1">
                <span className={`text-[10px] px-1.5 py-0.5 rounded ${
                  f.fact_type === 'user' ? 'bg-blue-900 text-blue-300' :
                  f.fact_type === 'feedback' ? 'bg-orange-900 text-orange-300' :
                  f.fact_type === 'reference' ? 'bg-green-900 text-green-300' :
                  'bg-gray-700 text-gray-300'
                }`}>{f.fact_type}</span>
                <span className="text-xs font-medium text-gray-300 truncate flex-1">{f.name}</span>
                <button onClick={() => deleteFact(f.name)}
                  className="text-gray-600 hover:text-red-400 text-xs opacity-0 group-hover:opacity-100 transition-opacity">
                  🗑
                </button>
              </div>
              <p className="text-xs text-gray-500 truncate">{f.description}</p>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
