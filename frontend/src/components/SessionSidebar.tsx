import { useEffect, useState, useRef } from 'react';
import { useStore } from '../store';
import appIcon from '@/assets/icons/app/everevo.svg';

type SettingsTab = 'llm' | 'routing' | 'character';

const SETTINGS_ITEMS: { key: SettingsTab; icon: string; label: string }[] = [
  { key: 'llm', icon: '⚙️', label: '大语言模型' },
  { key: 'routing', icon: '🧭', label: '模型路由' },
  { key: 'character', icon: '🎭', label: '人格声音' },
];

export default function SessionSidebar({
  view,
  settingsTab,
  onSettingsTabChange,
}: {
  view: 'chat' | 'settings' | 'bootstrap' | 'devpanel';
  settingsTab: SettingsTab;
  onSettingsTabChange: (tab: SettingsTab) => void;
}) {
  const {
    sessions, sessionsLoading, activeSessionId,
    loadSessions, createSession, deleteSession, switchSession,
    archiveSession, pinSession, renameSession,
  } = useStore();

  useEffect(() => { loadSessions(); }, []);

  return (
    <aside className="w-[260px] flex flex-col bg-sidebar border-r border-sidebar-border shrink-0 overflow-hidden">
      {/* Header */}
      <div className="p-3 border-b border-sidebar-border flex items-center gap-2">
        <img src={appIcon} alt="" className="w-5 h-5 shrink-0" style={{ imageRendering: 'pixelated' }} />
        <span className="text-xs font-bold text-sidebar-foreground font-mono tracking-widest uppercase">EverEvo</span>
      </div>

      {view === 'chat' ? (
        <>
          {/* New session */}
          <div className="p-2 border-b border-sidebar-border">
            <button onClick={createSession}
              className="w-full text-left bg-primary hover:bg-primary/90 text-primary-foreground text-xs font-medium py-2 px-3 transition-colors
                         shadow-[inset_0_1px_0_0_rgba(255,255,255,0.15),inset_0_-1px_0_0_rgba(0,0,0,0.1)]
                         active:shadow-[inset_0_1px_0_0_rgba(0,0,0,0.1),inset_0_-1px_0_0_rgba(255,255,255,0.08)] active:translate-y-px">
              + 新建对话
            </button>
          </div>

          {/* Session list */}
          <nav className="flex-1 overflow-y-auto">
            {sessionsLoading && sessions.length === 0 && (
              <div className="p-4 text-center">
                <div className="animate-spin w-5 h-5 border-2 border-primary border-t-transparent mx-auto" />
              </div>
            )}
            {!sessionsLoading && sessions.length === 0 && (
              <p className="text-xs text-muted-foreground text-center p-4">暂无对话</p>
            )}
            {sessions.map((s) => (
              <SessionRow
                key={s.id}
                session={s}
                active={activeSessionId === s.id}
                onSelect={() => switchSession(s.id)}
                onDelete={() => deleteSession(s.id)}
                onArchive={() => archiveSession(s.id)}
                onPin={() => pinSession(s.id)}
                onRename={(t) => renameSession(s.id, t)}
              />
            ))}
          </nav>
        </>
      ) : (
        /* Settings sub-nav */
        <nav className="flex-1 overflow-y-auto">
          {SETTINGS_ITEMS.map((item) => (
            <div
              key={item.key}
              onClick={() => onSettingsTabChange(item.key)}
              className={`flex items-center gap-2 px-3 py-2.5 cursor-pointer border-b border-sidebar-border/50 transition-all duration-300 border-l-[3px] ${
                settingsTab === item.key
                  ? 'bg-white/10'
                  : 'hover:bg-sidebar-accent/50'
              }`}
              style={{ borderLeftColor: settingsTab === item.key ? 'var(--warning)' : 'transparent' }}
            >
              <span className="text-sm shrink-0">{item.icon}</span>
              <span className="text-xs text-sidebar-foreground">{item.label}</span>
            </div>
          ))}
        </nav>
      )}
    </aside>
  );
}

// ── Session row ────────────────────────────────────────────────────

function SessionRow({
  session, active, onSelect, onDelete, onArchive, onPin, onRename,
}: {
  session: { id: string; title: string; last_message: string | null; created_at: string; updated_at: string; message_count: number; pinned?: boolean };
  active: boolean; onSelect: () => void; onDelete: () => void;
  onArchive: () => void; onPin: () => void; onRename: (t: string) => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editTitle, setEditTitle] = useState(session.title);
  const inputRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  // Close dropdown on outside click
  useEffect(() => {
    if (!menuOpen) return;
    const handler = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [menuOpen]);

  const preview = session.last_message
    ? session.last_message.length > 20 ? session.last_message.slice(0, 20) + '...' : session.last_message
    : '新对话';

  const createdAt = fmtDate(session.created_at);
  const lastActive = active ? 'now' : fmtSince(session.updated_at);

  const handleRename = () => {
    setEditing(true);
    setEditTitle(session.title);
    setTimeout(() => inputRef.current?.select(), 50);
  };

  const commitRename = () => {
    const t = editTitle.trim();
    if (t && t !== session.title) onRename(t);
    setEditing(false);
  };

  return (
    <div
      onClick={onSelect}
      className={`group px-3 py-2 cursor-pointer border-b border-sidebar-border/50 transition-all duration-300 border-l-[3px] ${
        active
          ? 'bg-white/10'
          : 'hover:bg-sidebar-accent/50'
      }`}
      style={{ borderLeftColor: active ? 'var(--warning)' : 'transparent' }}
    >
      {/* Top row: title + ⋮ */}
      <div className="flex items-center justify-between gap-1">
        {editing ? (
          <input
            ref={inputRef}
            value={editTitle}
            onChange={(e) => setEditTitle(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') commitRename(); if (e.key === 'Escape') setEditing(false); }}
            onBlur={commitRename}
            onClick={(e) => e.stopPropagation()}
            className="flex-1 bg-background border border-border text-xs px-1 py-0.5 min-w-0"
          />
        ) : (
          <span className="text-xs text-sidebar-foreground truncate flex-1 min-w-0">
            {session.pinned && '📌 '}{session.title}
          </span>
        )}

        {/* ⋮ dropdown */}
        <div className="relative shrink-0" ref={menuRef}>
          <button
            onClick={(e) => { e.stopPropagation(); setMenuOpen(!menuOpen); }}
            className={`shrink-0 w-5 h-5 flex items-center justify-center text-muted-foreground hover:text-sidebar-foreground text-sm leading-none transition-opacity ${menuOpen ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'}`}
          >
            ⋮
          </button>
          {menuOpen && (
            <div
              className="absolute right-0 top-full mt-1 w-28 bg-background border border-border shadow-xl z-50 py-0.5"
              onClick={(e) => e.stopPropagation()}
            >
              <DropItem icon="📌" label={session.pinned ? '取消置顶' : '置顶'} onClick={() => { onPin(); setMenuOpen(false); }} />
              <DropItem icon="✏️" label="重命名" onClick={() => { handleRename(); setMenuOpen(false); }} />
              {session.message_count > 0 && (
                <DropItem icon="📦" label="归档" onClick={() => { onArchive(); setMenuOpen(false); }} />
              )}
              <DropItem icon="🗑️" label="删除" onClick={() => { onDelete(); setMenuOpen(false); }} danger />
            </div>
          )}
        </div>
      </div>

      {/* Preview text */}
      <p className="text-[10px] text-muted-foreground truncate mt-0.5">{preview}</p>

      {/* Workspace indicator */}
      {(session as any).workspace_dir ? (
        <p className="text-[9px] text-muted-foreground/50 truncate mt-0.5">📁 {(session as any).workspace_dir}</p>
      ) : (
        <p className="text-[9px] text-muted-foreground/40 mt-0.5">🏖️ sandbox</p>
      )}

      {/* Time row */}
      <div className="flex gap-2 mt-1 text-[9px] text-muted-foreground/60">
        <span>{createdAt}</span>
        <span>·</span>
        <span className={active ? 'text-warning' : ''}>{lastActive}</span>
      </div>
    </div>
  );
}

function DropItem({ icon, label, onClick, danger }: { icon: string; label: string; onClick: () => void; danger?: boolean }) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center gap-1.5 px-3 py-1.5 text-[11px] transition-colors ${
        danger ? 'text-destructive hover:bg-destructive/10' : 'text-muted-foreground hover:bg-accent hover:text-foreground'
      }`}
    >
      <span className="w-4 text-center shrink-0">{icon}</span>
      <span>{label}</span>
    </button>
  );
}

function fmtDate(iso: string) {
  try {
    const d = new Date(iso);
    return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
  } catch { return ''; }
}
function fmtSince(iso: string) {
  try {
    const d = new Date(iso);
    const diff = Date.now() - d.getTime();
    if (diff < 60000) return '刚刚';
    if (diff < 3600000) return `${Math.floor(diff / 60000)}m`;
    if (diff < 86400000) return `${Math.floor(diff / 3600000)}h`;
    if (diff < 604800000) return `${Math.floor(diff / 86400000)}d`;
    return `${Math.floor(diff / 604800000)}w`;
  } catch { return ''; }
}
