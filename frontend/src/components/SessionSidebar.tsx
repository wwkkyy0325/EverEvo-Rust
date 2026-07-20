import { useEffect } from 'react';
import { useStore } from '../store';

export default function SessionSidebar() {
  const {
    sessions, sessionsLoading, activeSessionId,
    loadSessions, createSession, deleteSession, switchSession,
  } = useStore();

  useEffect(() => { loadSessions(); }, []);

  return (
    <aside className="w-64 h-screen flex flex-col bg-gray-950 border-r border-gray-800 shrink-0">
      {/* Header */}
      <div className="p-3 border-b border-gray-800 flex items-center justify-between">
        <span className="text-sm font-bold text-gray-300">EverEvo</span>
        <button
          onClick={createSession}
          className="text-lg text-gray-400 hover:text-white leading-none px-1"
          title="新建对话"
        >
          +
        </button>
      </div>

      {/* Session list */}
      <nav className="flex-1 overflow-y-auto">
        {sessionsLoading && sessions.length === 0 && (
          <div className="p-4 text-center">
            <div className="animate-spin w-5 h-5 border-2 border-blue-500 border-t-transparent rounded-full mx-auto" />
          </div>
        )}
        {!sessionsLoading && sessions.length === 0 && (
          <p className="text-xs text-gray-600 text-center p-4">暂无对话，点击 + 创建</p>
        )}
        {sessions.map((s) => (
          <div
            key={s.id}
            onClick={() => switchSession(s.id)}
            className={`group px-3 py-2.5 cursor-pointer border-b border-gray-900/50 transition-colors ${
              activeSessionId === s.id
                ? 'bg-blue-900/30 border-l-2 border-l-blue-500'
                : 'hover:bg-gray-900 border-l-2 border-l-transparent'
            }`}
          >
            <div className="flex items-start justify-between gap-1">
              <div className="min-w-0 flex-1">
                <p className={`text-xs truncate ${activeSessionId === s.id ? 'text-blue-200' : 'text-gray-300'}`}>
                  {s.title || 'New Session'}
                </p>
                {s.last_message && (
                  <p className="text-xs text-gray-600 truncate mt-0.5">{s.last_message}</p>
                )}
              </div>
              <button
                onClick={(e) => { e.stopPropagation(); deleteSession(s.id); }}
                className="opacity-0 group-hover:opacity-100 text-gray-600 hover:text-red-400 text-xs shrink-0 transition-opacity"
                title="删除"
              >
                ×
              </button>
            </div>
          </div>
        ))}
      </nav>

      {/* Footer */}
      <div className="p-2 border-t border-gray-800 text-xs text-gray-600 text-center">
        {sessions.length} 个对话
      </div>
    </aside>
  );
}
