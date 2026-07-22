// Domain Panel — manage knowledge domains and their documents.

import { useState, useEffect } from 'react';
import { useStore } from '../store';

interface DomainItem {
  id: string; name: string; description: string;
  document_count: number; related_ids: string[];
  created_at: string;
}

interface DocMeta {
  filename: string; size_bytes: number; modified: string;
}

export default function DomainPanel() {
  const { showDomain, toggleDomain } = useStore();
  const [domains, setDomains] = useState<DomainItem[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [docs, setDocs] = useState<DocMeta[]>([]);
  const [newName, setNewName] = useState('');
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState<any[]>([]);

  useEffect(() => { if (showDomain) loadDomains(); }, [showDomain]);

  const loadDomains = async () => {
    const res = await fetch('/api/domains');
    const json = await res.json();
    setDomains(json.data?.domains ?? []);
  };

  const selectDomain = async (id: string) => {
    setSelected(id);
    const res = await fetch(`/api/domains/${id}/documents`);
    const json = await res.json();
    setDocs(json.data?.documents ?? []);
  };

  const createDomain = async () => {
    if (!newName.trim()) return;
    await fetch('/api/domains', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: newName.trim() }),
    });
    setNewName('');
    loadDomains();
  };

  const scanInbox = async (id: string) => {
    await fetch(`/api/domains/${id}/scan`, { method: 'POST' });
    selectDomain(id);
    loadDomains();
  };

  const processInbox = async () => {
    await fetch('/api/domain/inbox/process', { method: 'POST' });
    loadDomains();
  };

  const searchAll = async () => {
    if (!searchQuery.trim()) return;
    const res = await fetch('/api/domain/search', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ query: searchQuery.trim() }),
    });
    const json = await res.json();
    setSearchResults(json.data?.results ?? []);
  };

  const deleteDomain = async (id: string) => {
    await fetch(`/api/domains/${id}`, { method: 'DELETE' });
    if (selected === id) setSelected(null);
    loadDomains();
  };

  if (!showDomain) return null;

  return (
    <div className="fixed right-0 top-0 h-screen w-80 bg-background border-l border-border z-40 overflow-y-auto shadow-2xl">
      <div className="p-4">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-bold text-foreground">📚 Domains ({domains.length})</h2>
          <button onClick={toggleDomain} className="text-muted-foreground hover:text-foreground text-lg">&times;</button>
        </div>

        {/* Create */}
        <div className="flex gap-1 mb-3">
          <input value={newName} onChange={e => setNewName(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && createDomain()}
            placeholder="New domain name..."
            className="flex-1 bg-secondary border border-border rounded px-2 py-1 text-xs text-foreground
                       placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
          <button onClick={createDomain} className="bg-primary hover:bg-primary/90 text-primary-foreground px-2 py-1 rounded text-xs">+</button>
        </div>

        {/* Actions */}
        <div className="flex gap-2 mb-3">
          <button onClick={processInbox} className="text-xs bg-thinking/30 hover:bg-thinking/40 text-thinking-foreground px-2 py-1 rounded flex-1">📥 Process Inbox</button>
          <button onClick={loadDomains} className="text-xs text-muted-foreground hover:text-foreground px-1">🔄</button>
        </div>

        {/* Search */}
        <div className="flex gap-1 mb-3">
          <input value={searchQuery} onChange={e => setSearchQuery(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && searchAll()}
            placeholder="Search all domains..."
            className="flex-1 bg-secondary border border-border rounded px-2 py-1 text-xs text-foreground
                       placeholder:text-muted-foreground focus:outline-none focus:border-primary" />
        </div>

        {/* Search results */}
        {searchResults.length > 0 && (
          <div className="mb-3 space-y-1">
            {searchResults.map((r: any, i: number) => (
              <div key={i} className="bg-secondary rounded p-2 text-xs">
                <span className="text-primary font-medium">{r.domain_name}</span>
                <span className="text-muted-foreground ml-2">{r.match_count} matches</span>
              </div>
            ))}
          </div>
        )}

        {/* Domain list */}
        <div className="space-y-1">
          {domains.map((d) => (
            <div key={d.id} className={`rounded p-2 cursor-pointer border ${
              selected === d.id ? 'bg-accent border-primary' : 'bg-secondary border-border/50 hover:bg-accent/50'
            }`}>
              <div onClick={() => selectDomain(d.id)} className="flex items-center justify-between">
                <div>
                  <span className="text-xs font-medium text-foreground">{d.name}</span>
                  <span className="text-[10px] text-muted-foreground ml-2">{d.document_count} docs</span>
                </div>
                <button onClick={(e) => { e.stopPropagation(); deleteDomain(d.id); }}
                  className="text-muted-foreground hover:text-destructive text-xs">🗑</button>
              </div>
              {selected === d.id && (
                <div className="mt-2 pt-2 border-t border-border">
                  <button onClick={() => scanInbox(d.id)}
                    className="text-[10px] bg-success/30 hover:bg-success/40 text-success-foreground px-2 py-0.5 rounded mb-2">📥 Scan Inbox</button>
                  {docs.map((doc, i) => (
                    <div key={i} className="text-[10px] text-muted-foreground truncate">{doc.filename} ({doc.size_bytes}B)</div>
                  ))}
                  {docs.length === 0 && <p className="text-[10px] text-muted-foreground">No documents</p>}
                </div>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
