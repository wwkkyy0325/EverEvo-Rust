// Zustand store — sessions + active chat state.
import { create } from 'zustand';

// ── Types ───────────────────────────────────────────────────────────────

export interface SessionItem {
  id: string; title: string; created_at: string; updated_at: string;
  message_count: number; last_message: string | null; pinned?: boolean;
  /** Session mode: "interactive" | "background" (daemon). */
  mode?: string;
  /** Session state: "idle" | "running" | "completed" | "failed". */
  state?: string;
}

export interface ToolInfo {
  name: string;
  description: string;
  source: string; // "builtin" | "mcp:<server>"
}

export interface McpServerInfo {
  name: string;
  status: string; // "connected" | "busy" | "dead"
  tools?: number;
  tool_names?: string[];
  server?: string;
  note?: string;
}

const ARCHIVED_KEY = 'everevo_archived_ids';
function loadArchivedIds(): string[] {
  try { return JSON.parse(localStorage.getItem(ARCHIVED_KEY) || '[]'); } catch { return []; }
}
function saveArchivedIds(ids: string[]) { localStorage.setItem(ARCHIVED_KEY, JSON.stringify(ids)); }

export interface ContentBlock {
  index: number;
  type: 'thinking' | 'tool_use' | 'text';
  thinking?: string;
  toolId?: string; toolName?: string; toolInput?: string;
  toolResult?: string; toolError?: boolean;
  text?: string;
}

export interface AuditRecord {
  timestamp: string; command: string; shell: string; exit_code: number;
  duration_ms: number; permission_level: string; was_confirmed: boolean;
  decision: string; stdout_len: number; stderr_len: number;
}

export interface ConfirmRequest { sessionId: string; command: string; reason: string; }

export interface MessageItem {
  id: string;
  role: 'user' | 'assistant' | 'system' | 'tool';
  content: string;
  tool_calls?: unknown;
  tool_call_id?: string | null;
  thinking?: string;
  /** Ordered content blocks. When present, rendering uses these instead of
   *  the legacy thinking/tool_calls/content fields. */
  blocks?: ContentBlock[];
  /** Serialized blocks from DB (parsed into `blocks` on load). */
  blocks_json?: unknown;
  /** Index of the block currently receiving streaming deltas (-1 = none). */
  activeBlockIdx?: number;
  created_at: string;
}

interface ChatState {
  sessions: SessionItem[]; sessionsLoading: boolean;
  activeSessionId: string | null; archivedIds: string[];
  messages: MessageItem[]; messagesLoading: boolean;
  historyCursor: string | null; hasMoreHistory: boolean;
  sandboxShell: string; sandboxLevel: string; sandboxPermissionKey: string;
  availableLevels: Array<{ key: string; label: string }>; activeSessions: number;
  toolCount: number;
  /** Full tool list from GET /api/tools (built-in + MCP). */
  tools: ToolInfo[];
  mcpServers: McpServerInfo[];
  /** Server health features set (autocompact, mcp_reconnect, code_search, etc.). */
  features: Record<string, boolean>;
  /** LLM config status. */
  llmConfigured: boolean;
  /** Number of configured LLM providers. */
  llmCount: number;
  /** Configured LLM provider IDs. */
  llmProviderIds: string[];
  /** Server version string. */
  serverVersion: string;

  // Streaming — draft message id points to an in-progress assistant message in messages[]
  streaming: boolean;
  draftId: string | null;
  abortController: AbortController | null;

  // TodoWrite state — updated when TodoWrite tool completes
  todos: Array<{ content: string; status: string; activeForm: string }>;
  // Sub-agent task tracking
  subagentTasks: Array<{ id: string; description: string; status: string; result?: string }>;
  // UI toggles
  showMemory: boolean;
  showAudit: boolean;

  confirmRequest: ConfirmRequest | null;
  auditRecords: AuditRecord[]; auditTotal: number;

  // ── Actions ──
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
  loadAudit: (sessionId: string) => Promise<void>;
  confirmCommand: (approved: boolean) => Promise<void>;
  abortStream: () => void;
  /** Load tool list + server health in one call. */
  loadHealth: () => Promise<void>;
  /** Get session mode/state. */
  loadSessionStatus: (id: string) => Promise<{ mode: string; state: string } | null>;
  /** Reconnect an MCP server. */
  reconnectMcpServer: (name: string) => Promise<void>;
  /** Current workspace path (null = no workspace set). */
  workspacePath: string | null;
  /** Set the workspace directory. */
  setWorkspace: (path: string) => Promise<void>;
  /** Whether the current session is in plan mode (read-only exploration). */
  planMode: boolean;
  /** The task being planned in plan mode. */
  planTask: string | null;
  /** Enter or exit plan mode. */
  setPlanMode: (active: boolean, task?: string | null) => void;
}

// ── Store ───────────────────────────────────────────────────────────────

export const useStore = create<ChatState>((set, get) => ({
  sessions: [], sessionsLoading: true, activeSessionId: null,
  archivedIds: loadArchivedIds(),
  messages: [], messagesLoading: false, historyCursor: null, hasMoreHistory: false,
  sandboxShell: '...', sandboxLevel: '...', sandboxPermissionKey: 'semi_auto',
  availableLevels: [
    { key: 'read_only', label: '只读' }, { key: 'fully_manual', label: '纯手动' },
    { key: 'semi_auto', label: '半自动' }, { key: 'fully_auto', label: '全自动' },
  ],
  activeSessions: 0, toolCount: 22, tools: [], mcpServers: [],
  features: {}, llmConfigured: false, llmCount: 0, llmProviderIds: [], serverVersion: '',
  auditRecords: [], auditTotal: 0,

  streaming: false, draftId: null, abortController: null,
  todos: [], subagentTasks: [], showMemory: false, showAudit: false,
  confirmRequest: null, workspacePath: null,
  planMode: false, planTask: null,

  // ── Session list ──────────────────────────────────────────────────
  loadSessions: async () => {
    set({ sessionsLoading: true });
    try {
      const res = await fetch('/api/sessions?limit=50');
      const json = await res.json();
      const raw: SessionItem[] = (json.data ?? []).map((s: any) => ({
        ...s,
        mode: s.mode ?? 'interactive',
        state: s.state ?? 'idle',
      }));
      const { archivedIds, sessions: existing } = get();
      const archivedSet = new Set(archivedIds);
      const sorted = raw
        .filter((s) => !archivedSet.has(s.id))
        .map((s) => { const old = existing.find((e) => e.id === s.id); return { ...s, pinned: old?.pinned ?? false }; })
        .sort((a, b) => { if (a.pinned && !b.pinned) return -1; if (!a.pinned && b.pinned) return 1; return new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime(); });
      set({ sessions: sorted, sessionsLoading: false });
    } catch { set({ sessionsLoading: false }); }
  },

  createSession: async () => {
    const { sessions, archivedIds } = get();
    const archivedSet = new Set(archivedIds);
    const empty = sessions.find((s) => s.message_count === 0 && !archivedSet.has(s.id));
    if (empty) { await get().switchSession(empty.id); return empty.id; }
    const res = await fetch('/api/sessions', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: 'New Session' }) });
    const json = await res.json();
    const id = json.data.id as string;
    await get().loadSessions(); await get().switchSession(id);
    return id;
  },

  deleteSession: async (id: string) => {
    await fetch(`/api/sessions/${id}`, { method: 'DELETE' });
    const { activeSessionId } = get();
    if (activeSessionId === id) set({ activeSessionId: null, messages: [], historyCursor: null, hasMoreHistory: false });
    await get().loadSessions();
  },

  archiveSession: (id: string) => {
    const { archivedIds, activeSessionId } = get();
    if (archivedIds.includes(id)) return;
    const next = [...archivedIds, id];
    saveArchivedIds(next);
    set((s) => ({ archivedIds: next, sessions: s.sessions.filter((ses) => ses.id !== id), ...(activeSessionId === id ? { activeSessionId: null, messages: [], historyCursor: null, hasMoreHistory: false } : {}) }));
  },

  pinSession: (id: string) => { set((s) => ({ sessions: s.sessions.map((ses) => ses.id === id ? { ...ses, pinned: !ses.pinned } : ses) })); },
  renameSession: async (id: string, title: string) => {
    set((s) => ({ sessions: s.sessions.map((ses) => ses.id === id ? { ...ses, title } : ses) }));
    try { await fetch(`/api/sessions/${id}`, { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title }) }); } catch { /* offline — local state is already updated */ }
  },

  // ── Messages ──────────────────────────────────────────────────────
  switchSession: async (id: string) => {
    const session = get().sessions.find((s) => s.id === id);
    const isEmpty = session != null && session.message_count === 0;
    set({ activeSessionId: id, messages: [], messagesLoading: !isEmpty, historyCursor: null, hasMoreHistory: false, streaming: false, draftId: null, todos: [], subagentTasks: [] });
    get().loadSandboxStatus();
    get().loadHealth();
    fetch(`/api/session/${id}/todos`).then(r => r.json()).then(j => { if (j.data?.todos) set({ todos: j.data.todos }); }).catch(() => {});
    if (isEmpty) { set({ messagesLoading: false }); return; }
    try {
      const res = await fetch(`/api/sessions/${id}/messages?limit=50`);
      const json = await res.json();
      const msgs: MessageItem[] = (json.data ?? []).map((m: any) => ({
        ...m,
        blocks: m.blocks_json ?? m.blocks ?? undefined,
      }));
      set({ messages: msgs.reverse(), messagesLoading: false, historyCursor: json.next_cursor ?? null, hasMoreHistory: json.has_more ?? false });
    } catch { set({ messagesLoading: false }); }
  },

  loadMoreMessages: async () => {
    const { activeSessionId, historyCursor, hasMoreHistory, messagesLoading } = get();
    if (!activeSessionId || !historyCursor || !hasMoreHistory || messagesLoading) return;
    set({ messagesLoading: true });
    try {
      const res = await fetch(`/api/sessions/${activeSessionId}/messages?before=${historyCursor}&limit=50`);
      const json = await res.json();
      const older: MessageItem[] = (json.data ?? []).map((m: any) => ({
        ...m,
        blocks: m.blocks_json ?? m.blocks ?? undefined,
      }));
      set({ messages: [...older.reverse(), ...get().messages], messagesLoading: false, historyCursor: json.next_cursor ?? null, hasMoreHistory: json.has_more ?? false });
    } catch { set({ messagesLoading: false }); }
  },

  loadAudit: async (sessionId: string) => {
    try { const res = await fetch(`/api/sandbox/sessions/${sessionId}/audit?limit=50`); const json = await res.json(); set({ auditRecords: json.data?.records ?? [], auditTotal: json.data?.total ?? 0 }); } catch { /* ignore */ }
  },

  abortStream: () => { const { abortController } = get(); if (abortController) abortController.abort(); },

  confirmCommand: async (approved: boolean) => {
    const req = get().confirmRequest; if (!req) return;
    try { await fetch(`/api/sandbox/sessions/${req.sessionId}/confirm`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ approved }) }); } catch { /* ignore */ }
    set({ confirmRequest: null });
  },

  setPermissionLevel: async (level: string) => {
    const { activeSessionId, availableLevels } = get(); if (!activeSessionId) return;
    try {
      await fetch(`/api/sandbox/sessions/${activeSessionId}/permission`, { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ level }) });
      const selected = availableLevels.find((l) => l.key === level);
      set({ sandboxPermissionKey: level, sandboxLevel: selected?.label ?? level });
    } catch { /* ignore */ }
  },

  loadSandboxStatus: async () => {
    try {
      const res = await fetch('/api/sandbox/status'); const json = await res.json();
      if (json.data) set({ sandboxShell: json.data.shell ?? 'none', sandboxLevel: json.data.permission_level ?? '—', sandboxPermissionKey: json.data.permission_key ?? 'semi_auto', availableLevels: json.data.available_levels ?? [{ key: 'read_only', label: '只读' }, { key: 'fully_manual', label: '纯手动' }, { key: 'semi_auto', label: '半自动' }, { key: 'fully_auto', label: '全自动' }], activeSessions: json.data.active_sessions ?? 0 });
    } catch { /* ignore */ }
  },

  // ── Send Message — Claude Code pattern: draft message IN messages[] ──
  sendMessage: async (text: string) => {
    const { activeSessionId, streaming } = get();
    if (!text.trim() || streaming) return;

    let sessionId = activeSessionId;
    if (!sessionId) sessionId = await get().createSession();

    // Push user message + draft assistant message
    const userMsg: MessageItem = { id: crypto.randomUUID(), role: 'user', content: text, created_at: new Date().toISOString() };
    const draftId = crypto.randomUUID();
    const draftMsg: MessageItem = { id: draftId, role: 'assistant', content: '', blocks: [], activeBlockIdx: -1, created_at: new Date().toISOString() };
    const ac = new AbortController();
    set((s) => ({ messages: [...s.messages, userMsg, draftMsg], streaming: true, draftId, abortController: ac }));

    // Helper: update the draft message's blocks in-place
    const updateDraft = (fn: (draft: MessageItem) => Partial<MessageItem>) => {
      set((s) => ({ messages: s.messages.map((m) => m.id === s.draftId ? { ...m, ...fn(m) } : m) }));
    };
    const getBlocks = (): ContentBlock[] => {
      const draft = get().messages.find((m) => m.id === get().draftId);
      return (draft?.blocks as ContentBlock[]) ?? [];
    };
    const lastBlock = (): ContentBlock | undefined => { const b = getBlocks(); return b[b.length - 1]; };

    try {
      const resp = await fetch('/api/chat', { signal: ac.signal, method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ session_id: sessionId, message: text }) });
      const reader = resp.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '', currentEvent = '', pendingNl = 0;

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (line.startsWith('event: ')) { currentEvent = line.slice(7).trim(); pendingNl = 0; continue; }
          if (!line.startsWith('data: ')) continue;
          const data = line.slice(6);
          if (!data) { if (pendingNl > 0) pendingNl++; continue; }

          const nl = '\n'.repeat(pendingNl);
          pendingNl = (currentEvent === 'thinking' || currentEvent === 'content_block_delta' || currentEvent === '' || currentEvent === 'token') ? 1 : 0;

          // ── content_block_start ──────────────────────────────────
          if (currentEvent === 'content_block_start') {
            try {
              const cb = JSON.parse(data);
              const block: ContentBlock = {
                index: cb.index as number,
                type: cb.content_block.type as ContentBlock['type'],
                thinking: cb.content_block.type === 'thinking' ? '' : undefined,
                text: cb.content_block.type === 'text' ? '' : undefined,
                toolId: cb.content_block.type === 'tool_use' ? (cb.content_block.id as string) : undefined,
                toolName: cb.content_block.type === 'tool_use' ? (cb.content_block.name as string) : undefined,
                toolInput: cb.content_block.type === 'tool_use' ? '' : undefined,
              };
              updateDraft(() => ({ blocks: [...getBlocks(), block], activeBlockIdx: cb.index as number }));
            } catch { /* ignore */ }
            continue;
          }

          // ── content_block_delta ──────────────────────────────────
          if (currentEvent === 'content_block_delta') {
            try {
              const d = JSON.parse(data);
              const idx = d.index as number;
              updateDraft(() => ({
                blocks: getBlocks().map((b) => {
                  if (b.index !== idx) return b;
                  if (d.delta.type === 'thinking_delta') return { ...b, thinking: (b.thinking || '') + (d.delta.thinking as string || '') };
                  if (d.delta.type === 'text_delta') return { ...b, text: (b.text || '') + nl + (d.delta.text as string || '') };
                  if (d.delta.type === 'input_json_delta') return { ...b, toolInput: (b.toolInput || '') + (d.delta.partial_json as string || '') };
                  return b;
                }),
              }));
            } catch { /* ignore */ }
            continue;
          }

          // ── content_block_stop ────────────────────────────────────
          if (currentEvent === 'content_block_stop') {
            // Mark block as no longer streaming — ChatBubble uses this
            // to decide which thinking block shows "思考中" vs "思考过程".
            updateDraft(() => ({ activeBlockIdx: -1 }));
            continue;
          }

          // ── tool_result ───────────────────────────────────────────
          if (currentEvent === 'tool_result') {
            try {
              const tr = JSON.parse(data);
              updateDraft(() => ({
                blocks: getBlocks().map((b) => {
                  if (b.type === 'tool_use' && b.toolId === tr.tool_use_id) {
                    // If this is a TodoWrite tool, parse the input as the new todo list
                    if (b.toolName === 'TodoWrite' && b.toolInput) {
                      try {
                        const input = JSON.parse(b.toolInput);
                        if (input.todos) set({ todos: input.todos });
                      } catch { /* ignore */ }
                    }
                    return { ...b, toolResult: tr.content as string || '', toolError: !!tr.is_error };
                  }
                  return b;
                }),
              }));
            } catch { /* ignore */ }
            continue;
          }

          // ── confirmation_required ──────────────────────────────────
          if (currentEvent === 'confirmation_required') {
            try {
              const cr = JSON.parse(data);
              set({ confirmRequest: { sessionId: cr.session_id as string, command: cr.command as string, reason: cr.reason as string } });
            } catch { /* ignore */ }
            continue;
          }

          // ── subagent_started / subagent_result ─────────────────────
          if (currentEvent === 'subagent_started') {
            try {
              const sa = JSON.parse(data);
              set((s) => ({ subagentTasks: [...(s.subagentTasks || []), { id: sa.id, description: sa.description, status: 'running' }] }));
            } catch { /* ignore */ }
            continue;
          }
          if (currentEvent === 'subagent_result') {
            try {
              const sr = JSON.parse(data);
              set((s) => ({ subagentTasks: (s.subagentTasks || []).map((t: any) => t.id === sr.id ? { ...t, status: 'done', result: sr.result } : t) }));
            } catch { /* ignore */ }
            continue;
          }

          // ── error event ────────────────────────────────────────────
          if (currentEvent === 'error') {
            updateDraft(() => ({ content: `⚠️ ${data}` }));
            continue;
          }

          // ── message_stop / done → finalize draft ──────────────────
          if (currentEvent === 'message_stop' || currentEvent === 'done') {
            let msgId = '';
            try { const dd = JSON.parse(data); if (dd?.session_id) msgId = dd.message_id as string || ''; } catch { /* ignore */ }

            // Copy blocks from draft
            const finalBlocks = getBlocks();
            const textContent = finalBlocks.filter((b: ContentBlock) => b.type === 'text').map((b: ContentBlock) => b.text || '').join('');

            set((s) => ({
              streaming: false, draftId: null, abortController: null, subagentTasks: [],
              messages: s.messages.map((m) => m.id === s.draftId
                ? { ...m, id: msgId || m.id, content: textContent, blocks: finalBlocks.length > 0 ? finalBlocks : undefined, activeBlockIdx: -1 }
                : m),
            }));
            get().loadSessions(); get().loadSandboxStatus();
            return;
          }

          // ── Legacy thinking / token events ─────────────────────────
          if (currentEvent === 'thinking') {
            const last = lastBlock();
            if (last && last.type === 'thinking') {
              updateDraft(() => ({ blocks: getBlocks().map((b, i, arr) => i === arr.length - 1 ? { ...b, thinking: (b.thinking || '') + data } : b) }));
            } else {
              updateDraft(() => ({ blocks: [...getBlocks(), { index: getBlocks().length, type: 'thinking', thinking: data }] }));
            }
            continue;
          }
          if (currentEvent === 'token') {
            const last = lastBlock();
            if (last && last.type === 'text') {
              updateDraft(() => ({ blocks: getBlocks().map((b, i, arr) => i === arr.length - 1 ? { ...b, text: (b.text || '') + nl + data } : b) }));
            } else {
              updateDraft(() => ({ blocks: [...getBlocks(), { index: getBlocks().length, type: 'text', text: nl + data }] }));
            }
            continue;
          }
        }
      }
    } catch (e: any) {
      if (e?.name === 'AbortError') {
        const finalBlocks = getBlocks();
        const textContent = finalBlocks.filter((b: ContentBlock) => b.type === 'text').map((b: ContentBlock) => b.text || '').join('');
        set((s) => ({
          streaming: false, draftId: null, abortController: null,
          subagentTasks: [],
          messages: s.messages.map((m) => m.id === s.draftId
            ? { ...m, content: textContent || '(已中断)', blocks: finalBlocks.length > 0 ? finalBlocks : undefined, activeBlockIdx: -1 }
            : m),
        }));
        return;
      }
      set({ streaming: false, draftId: null, abortController: null, subagentTasks: [] });
      set((s) => ({ messages: [...s.messages, { id: crypto.randomUUID(), role: 'assistant' as const, content: `连接错误: ${e}`, created_at: new Date().toISOString() }] }));
    }
  },

  // ── Health + tools discovery ────────────────────────────────────────
  loadHealth: async () => {
    try {
      const [healthRes, toolsRes, wsRes] = await Promise.all([
        fetch('/api/health'),
        fetch('/api/tools'),
        fetch('/api/workspace'),
      ]);
      const health = await healthRes.json();
      const tools = await toolsRes.json();
      const ws = await wsRes.json().catch(() => ({}));
      set({
        serverVersion: health.version ?? '',
        llmConfigured: health.llm?.any_available ?? false,
        llmCount: health.llm?.configured ?? 0,
        llmProviderIds: health.llm?.provider_ids ?? [],
        activeSessions: health.sessions?.active ?? 0,
        toolCount: tools.count ?? 19,
        tools: tools.tools ?? [],
        mcpServers: health.mcp_servers ?? [],
        features: health.features ?? {},
        workspacePath: ws.path ?? null,
      });
    } catch { /* ignore */ }
  },

  // ── Session status (mode + state for daemon sessions) ──────────────
  loadSessionStatus: async (id: string) => {
    try {
      const res = await fetch(`/api/sessions/${id}/status`);
      const json = await res.json();
      return { mode: json.mode ?? 'interactive', state: json.state ?? 'idle' };
    } catch { return null; }
  },

  // ── MCP server reconnect ────────────────────────────────────────────
  reconnectMcpServer: async (name: string) => {
    try {
      const res = await fetch(`/api/mcp/servers/${encodeURIComponent(name)}/reconnect`, { method: 'POST' });
      const json = await res.json();
      if (json.success) await get().loadHealth();
    } catch { /* ignore */ }
  },

  // ── Workspace management ───────────────────────────────────────────
  setWorkspace: async (path: string) => {
    try {
      await fetch('/api/workspace', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path }),
      });
      await get().loadHealth();
    } catch { /* ignore */ }
  },

  setPlanMode: (active: boolean, task?: string | null) => {
    set({ planMode: active, planTask: task ?? null });
  },
}));
