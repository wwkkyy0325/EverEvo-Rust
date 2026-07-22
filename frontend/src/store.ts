// Zustand store — sessions + active chat state.
//
// API contract (see crates/everevo-server/src/routes/):
//   GET  /api/sessions?limit=20&offset=0          → { data: SessionItem[], has_more, total }
//   POST /api/sessions { title }                   → { data: { id, title, ... } }
//   DELETE /api/sessions/{id}                       → { data: { deleted: true } }
//   GET  /api/sessions/{id}/messages?before=&limit=50 → { data: MessageItem[], next_cursor, has_more }
//   POST /api/chat  { session_id?, message }        → SSE stream

import { create } from 'zustand';

// ── Types ───────────────────────────────────────────────────────────────

export interface SessionItem {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  last_message: string | null;
  // Local-only fields (not persisted by backend)
  pinned?: boolean;
}

// ── Persisted local state ──────────────────────────────────────────────

const ARCHIVED_KEY = 'everevo_archived_ids';

function loadArchivedIds(): string[] {
  try { return JSON.parse(localStorage.getItem(ARCHIVED_KEY) || '[]'); } catch { return []; }
}
function saveArchivedIds(ids: string[]) {
  localStorage.setItem(ARCHIVED_KEY, JSON.stringify(ids));
}

export interface ToolCallEvent {
  id: string;
  name: string;
  arguments?: unknown;
  content?: string;
  is_error?: boolean;
  status: 'running' | 'done';
}

export interface AuditRecord {
  timestamp: string;
  command: string;
  shell: string;
  exit_code: number;
  duration_ms: number;
  permission_level: string;
  was_confirmed: boolean;
  decision: string;
  stdout_len: number;
  stderr_len: number;
}

export interface ConfirmRequest {
  sessionId: string;
  command: string;
  reason: string;
}

export interface MessageItem {
  id: string;
  role: 'user' | 'assistant' | 'system' | 'tool';
  content: string;
  tool_calls?: unknown;
  tool_call_id?: string | null;
  created_at: string;
}

interface ChatState {
  // Session list
  sessions: SessionItem[];
  sessionsLoading: boolean;
  activeSessionId: string | null;
  archivedIds: string[];

  // Messages for the active session
  messages: MessageItem[];
  messagesLoading: boolean;
  historyCursor: string | null;
  hasMoreHistory: boolean;

  // Sandbox status
  sandboxShell: string;
  sandboxLevel: string;
  sandboxPermissionKey: string;
  availableLevels: Array<{ key: string; label: string }>;
  activeSessions: number;

  // Streaming
  streaming: boolean;
  streamContent: string;
  thinkingContent: string;
  showThinking: boolean;

  // Tool execution display
  toolCalls: ToolCallEvent[];

  // Confirmation dialog (from sandbox SemiAuto mode)
  confirmRequest: ConfirmRequest | null;

  // Actions
  loadSessions: () => Promise<void>;
  createSession: () => Promise<string>;
  deleteSession: (id: string) => Promise<void>;
  archiveSession: (id: string) => void;
  pinSession: (id: string) => void;
  renameSession: (id: string, title: string) => void;
  switchSession: (id: string) => Promise<void>;
  loadMoreMessages: () => Promise<void>;
  sendMessage: (text: string) => Promise<void>;
  loadSandboxStatus: () => Promise<void>;
  setPermissionLevel: (level: string) => Promise<void>;

  // Audit
  auditRecords: AuditRecord[];
  auditTotal: number;
  showAudit: boolean;
  loadAudit: (sessionId: string) => Promise<void>;
  toggleAudit: () => void;

  // Memory panel
  showMemory: boolean;
  toggleMemory: () => void;

  // Domain panel
  showDomain: boolean;
  toggleDomain: () => void;

  // Confirmation
  confirmCommand: (approved: boolean) => Promise<void>;
}

// ── Store ───────────────────────────────────────────────────────────────

export const useStore = create<ChatState>((set, get) => ({
  sessions: [],
  sessionsLoading: true,
  activeSessionId: null,
  archivedIds: loadArchivedIds(),
  messages: [],
  messagesLoading: false,
  historyCursor: null,
  hasMoreHistory: false,
  sandboxShell: '...',
  sandboxLevel: '...',
  sandboxPermissionKey: 'semi_auto',
  availableLevels: [
    { key: 'read_only', label: '只读' },
    { key: 'fully_manual', label: '纯手动' },
    { key: 'semi_auto', label: '半自动' },
    { key: 'fully_auto', label: '全自动' },
  ],
  activeSessions: 0,
  auditRecords: [],
  auditTotal: 0,
  showAudit: false,
  showMemory: false,
  showDomain: false,

  streaming: false,
  streamContent: '',
  thinkingContent: '',
  showThinking: true,
  toolCalls: [],
  confirmRequest: null,

  // ── Session list ──────────────────────────────────────────────────
  loadSessions: async () => {
    set({ sessionsLoading: true });
    try {
      const res = await fetch('/api/sessions?limit=50');
      const json = await res.json();
      const raw: SessionItem[] = json.data ?? [];
      const { archivedIds, sessions: existing } = get();
      const archivedSet = new Set(archivedIds);
      const sorted = raw
        .filter((s) => !archivedSet.has(s.id))
        .map((s) => {
          const old = existing.find((e) => e.id === s.id);
          return { ...s, pinned: old?.pinned ?? false };
        })
        .sort((a, b) => {
          if (a.pinned && !b.pinned) return -1;
          if (!a.pinned && b.pinned) return 1;
          return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime();
        });
      set({ sessions: sorted, sessionsLoading: false });
    } catch {
      set({ sessionsLoading: false });
    }
  },

  createSession: async () => {
    // Reuse existing empty session if one is already present (prevent accidental duplicates)
    const { sessions, archivedIds } = get();
    const archivedSet = new Set(archivedIds);
    const empty = sessions.find((s) => s.message_count === 0 && !archivedSet.has(s.id));
    if (empty) {
      await get().switchSession(empty.id);
      return empty.id;
    }
    const res = await fetch('/api/sessions', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title: 'New Session' }),
    });
    const json = await res.json();
    const id = json.data.id as string;
    await get().loadSessions();
    await get().switchSession(id);
    return id;
  },

  deleteSession: async (id: string) => {
    await fetch(`/api/sessions/${id}`, { method: 'DELETE' });
    const { activeSessionId } = get();
    if (activeSessionId === id) {
      set({ activeSessionId: null, messages: [], historyCursor: null, hasMoreHistory: false });
    }
    await get().loadSessions();
  },

  archiveSession: (id: string) => {
    const { archivedIds, activeSessionId } = get();
    if (archivedIds.includes(id)) return;
    const next = [...archivedIds, id];
    saveArchivedIds(next);
    set((s) => ({
      archivedIds: next,
      sessions: s.sessions.filter((ses) => ses.id !== id),
      ...(activeSessionId === id ? { activeSessionId: null, messages: [], historyCursor: null, hasMoreHistory: false } : {}),
    }));
  },

  pinSession: (id: string) => {
    set((s) => ({
      sessions: s.sessions.map((ses) =>
        ses.id === id ? { ...ses, pinned: !ses.pinned } : ses
      ),
    }));
  },

  renameSession: (id: string, title: string) => {
    set((s) => ({
      sessions: s.sessions.map((ses) =>
        ses.id === id ? { ...ses, title } : ses
      ),
    }));
  },

  // ── Messages ──────────────────────────────────────────────────────
  switchSession: async (id: string) => {
    set({ activeSessionId: id, messages: [], messagesLoading: true, historyCursor: null, hasMoreHistory: false, streamContent: '', thinkingContent: '', showThinking: true, toolCalls: [], showAudit: false });
    get().loadSandboxStatus();
    try {
      const res = await fetch(`/api/sessions/${id}/messages?limit=50`);
      const json = await res.json();
      const msgs: MessageItem[] = json.data ?? [];
      // API returns newest first; reverse for chronological display
      set({
        messages: msgs.reverse(),
        messagesLoading: false,
        historyCursor: json.next_cursor ?? null,
        hasMoreHistory: json.has_more ?? false,
      });
    } catch {
      set({ messagesLoading: false });
    }
  },

  loadMoreMessages: async () => {
    const { activeSessionId, historyCursor, hasMoreHistory, messagesLoading } = get();
    if (!activeSessionId || !historyCursor || !hasMoreHistory || messagesLoading) return;

    set({ messagesLoading: true });
    try {
      const res = await fetch(
        `/api/sessions/${activeSessionId}/messages?before=${historyCursor}&limit=50`
      );
      const json = await res.json();
      const older: MessageItem[] = json.data ?? [];
      set({
        // Prepend older messages (they come newest-first from API, reverse for chronological order)
        messages: [...older.reverse(), ...get().messages],
        messagesLoading: false,
        historyCursor: json.next_cursor ?? null,
        hasMoreHistory: json.has_more ?? false,
      });
    } catch {
      set({ messagesLoading: false });
    }
  },

  // ── Send ──────────────────────────────────────────────────────────
  loadAudit: async (sessionId: string) => {
    try {
      const res = await fetch(`/api/sandbox/sessions/${sessionId}/audit?limit=50`);
      const json = await res.json();
      set({ auditRecords: json.data?.records ?? [], auditTotal: json.data?.total ?? 0, showAudit: true });
    } catch { /* ignore */ }
  },
  toggleAudit: () => set((s) => ({ showAudit: !s.showAudit })),
  toggleMemory: () => set((s) => ({ showMemory: !s.showMemory })),
  toggleDomain: () => set((s) => ({ showDomain: !s.showDomain })),

  // ── Confirmation ──────────────────────────────────────────────────
  confirmCommand: async (approved: boolean) => {
    const req = get().confirmRequest;
    if (!req) return;

    try {
      await fetch(`/api/sandbox/sessions/${req.sessionId}/confirm`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ approved }),
      });
    } catch {
      // If the request fails, clear anyway (timeout, etc.)
    }
    // Clear the confirmation dialog regardless
    set({ confirmRequest: null });
  },

  setPermissionLevel: async (level: string) => {
    const { activeSessionId, availableLevels } = get();
    if (!activeSessionId) return;

    try {
      await fetch(`/api/sandbox/sessions/${activeSessionId}/permission`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ level }),
      });
      // Update local state immediately for responsive UI
      const selected = availableLevels.find((l) => l.key === level);
      set({
        sandboxPermissionKey: level,
        sandboxLevel: selected?.label ?? level,
      });
    } catch {
      // Silently fail — status refresh will correct it
    }
  },

  loadSandboxStatus: async () => {
    try {
      const res = await fetch('/api/sandbox/status');
      const json = await res.json();
      if (json.data) {
        set({
          sandboxShell: json.data.shell ?? 'none',
          sandboxLevel: json.data.permission_level ?? '—',
          sandboxPermissionKey: json.data.permission_key ?? 'semi_auto',
          availableLevels: json.data.available_levels ?? [
            { key: 'read_only', label: '只读' },
            { key: 'fully_manual', label: '纯手动' },
            { key: 'semi_auto', label: '半自动' },
            { key: 'fully_auto', label: '全自动' },
          ],
          activeSessions: json.data.active_sessions ?? 0,
        });
      }
    } catch { /* sandbox not available yet */ }
  },

  sendMessage: async (text: string) => {
    const { activeSessionId, streaming } = get();
    if (!text.trim() || streaming) return;

    let sessionId = activeSessionId;

    // Auto-create session if none active
    if (!sessionId) {
      sessionId = await get().createSession();
    }

    // Optimistic user message
    const userMsg: MessageItem = {
      id: crypto.randomUUID(),
      role: 'user',
      content: text,
      created_at: new Date().toISOString(),
    };
    set((s) => ({
      messages: [...s.messages, userMsg],
      streaming: true,
      streamContent: '',
      thinkingContent: '',
      showThinking: true,
      toolCalls: [],
    }));

    try {
      const resp = await fetch('/api/chat', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ session_id: sessionId, message: text }),
      });
      const reader = resp.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '';
      let full = '';
      let currentEvent = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        // Parse SSE events
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (line.startsWith('event: ')) {
            currentEvent = line.slice(7).trim();
            continue;
          }
          if (line.startsWith('data: ')) {
            const data = line.slice(6);
            if (!data) continue;

            // Tool start event
            if (currentEvent === 'tool_start') {
              try {
                const tc = JSON.parse(data);
                set((s) => ({ toolCalls: [...s.toolCalls, { id: tc.id, name: tc.name, arguments: tc.arguments, status: 'running' as const }] }));
              } catch { /* ignore parse errors */ }
              continue;
            }
            // Tool end event
            if (currentEvent === 'tool_end') {
              try {
                const tc = JSON.parse(data);
                set((s) => ({
                  toolCalls: s.toolCalls.map((t) =>
                    t.id === tc.id ? { ...t, content: tc.content, is_error: tc.is_error, status: 'done' as const } : t
                  ),
                }));
              } catch { /* ignore */ }
              continue;
            }
            // Confirmation required event — sandbox SemiAuto gate
            if (currentEvent === 'confirmation_required') {
              try {
                const cr = JSON.parse(data);
                set({
                  confirmRequest: {
                    sessionId: cr.session_id,
                    command: cr.command,
                    reason: cr.reason,
                  },
                });
              } catch { /* ignore */ }
              continue;
            }

            // Thinking event — chain-of-thought tokens
            if (currentEvent === 'thinking') {
              set((s) => ({ thinkingContent: s.thinkingContent + data }));
              continue;
            }

            // Token event — final response
            try {
              const parsed = JSON.parse(data);
              if (typeof parsed === 'string') {
                full += parsed;
                set({ streamContent: full });
              } else if (parsed.session_id) {
                // done event
                set({ streaming: false, streamContent: '', showThinking: false });
                set((s) => ({
                  messages: [
                    ...s.messages,
                    {
                      id: parsed.message_id ?? crypto.randomUUID(),
                      role: 'assistant',
                      content: full,
                      created_at: new Date().toISOString(),
                    },
                  ],
                }));
                get().loadSessions();
                get().loadSandboxStatus();
                return;
              }
            } catch {
              full += data;
              set({ streamContent: full });
            }
          }
        }
      }
    } catch (e) {
      set({ streaming: false, streamContent: '' });
      set((s) => ({
        messages: [
          ...s.messages,
          {
            id: crypto.randomUUID(),
            role: 'assistant',
            content: `连接错误: ${e}`,
            created_at: new Date().toISOString(),
          },
        ],
      }));
    }
  },
}));
