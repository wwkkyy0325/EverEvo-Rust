import { useState, useRef, useEffect, useCallback } from 'react';
import { useStore } from '../store';

export default function ChatView() {
  const {
    activeSessionId, sessions,
    messages, messagesLoading, hasMoreHistory,
    streaming, streamContent, thinkingContent, showThinking, toolCalls,
    sandboxShell, sandboxLevel, sandboxPermissionKey, availableLevels, activeSessions,
    loadMoreMessages, sendMessage, loadSandboxStatus, setPermissionLevel, loadAudit,
    toggleMemory, toggleDomain,
  } = useStore();

  useEffect(() => { loadSandboxStatus(); }, []);
  const toggleThinking = () => useStore.setState((s) => ({ showThinking: !s.showThinking }));

  const [input, setInput] = useState('');
  const [showLevelDropdown, setShowLevelDropdown] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const chatContainerRef = useRef<HTMLDivElement>(null);
  const loadingRef = useRef(false);

  const activeSession = sessions.find((s) => s.id === activeSessionId);

  // Auto-scroll to bottom on new messages / streaming
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, streamContent]);

  // Infinite scroll — load older messages when scrolling to top
  const handleScroll = useCallback(() => {
    const el = chatContainerRef.current;
    if (!el || loadingRef.current) return;
    if (el.scrollTop < 60 && hasMoreHistory && !messagesLoading) {
      loadingRef.current = true;
      // Remember scroll height before loading
      const prevHeight = el.scrollHeight;
      loadMoreMessages().finally(() => {
        loadingRef.current = false;
        // Restore scroll position after prepending
        requestAnimationFrame(() => {
          if (el) {
            el.scrollTop = el.scrollHeight - prevHeight;
          }
        });
      });
    }
  }, [hasMoreHistory, messagesLoading, loadMoreMessages]);

  const handleSend = () => {
    if (!input.trim() || streaming) return;
    sendMessage(input.trim());
    setInput('');
  };

  // Empty state — no session selected
  if (!activeSession) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-center">
          <p className="text-gray-500 text-lg mb-2">EverEvo</p>
          <p className="text-gray-600 text-sm">选择一个对话，或创建新对话开始</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col h-screen">
      {/* Header */}
      <header className="px-4 py-3 border-b border-gray-800 bg-gray-950/80 backdrop-blur shrink-0 flex items-center justify-between">
        <div>
          <h1 className="text-sm font-semibold text-gray-200 truncate">{activeSession.title}</h1>
          <p className="text-xs text-gray-600">{activeSession.message_count} 条消息</p>
        </div>
        <button
          onClick={() => { activeSessionId && loadAudit(activeSessionId); }}
          className="text-xs text-gray-500 hover:text-purple-400 px-2 py-1 rounded border border-gray-700 hover:border-purple-700 transition-colors"
        >
          审计
        </button>
        <button
          onClick={toggleMemory}
          className="text-xs text-gray-500 hover:text-yellow-400 px-2 py-1 rounded border border-gray-700 hover:border-yellow-700 transition-colors"
        >
          🧠 记忆
        </button>
        <button
          onClick={toggleDomain}
          className="text-xs text-gray-500 hover:text-green-400 px-2 py-1 rounded border border-gray-700 hover:border-green-700 transition-colors"
        >
          📚 领域
        </button>
      </header>

      {/* Messages */}
      <main
        ref={chatContainerRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto px-4 py-3 space-y-3"
      >
        {/* Loading older messages indicator */}
        {messagesLoading && (
          <div className="flex justify-center py-2">
            <div className="animate-spin w-4 h-4 border-2 border-blue-500 border-t-transparent rounded-full" />
          </div>
        )}
        {hasMoreHistory && !messagesLoading && (
          <p className="text-xs text-gray-600 text-center py-1">向上滚动加载更多</p>
        )}

        {messages.length === 0 && !streaming && (
          <p className="text-gray-500 text-center mt-8 text-sm">发送消息开始对话</p>
        )}

        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`p-3 rounded-lg max-w-[85%] text-sm ${
              msg.role === 'user'
                ? 'bg-blue-900/40 ml-auto'
                : 'bg-gray-800'
            }`}
          >
            <p className="whitespace-pre-wrap">{msg.content}</p>
          </div>
        ))}

        {/* Thinking panel — collapsible chain-of-thought */}
        {streaming && thinkingContent && (
          <div className="max-w-[90%] mx-auto">
            <button
              onClick={toggleThinking}
              className="flex items-center gap-2 text-xs text-purple-400 hover:text-purple-300 py-1 w-full text-left"
            >
              <span>{showThinking ? '▼' : '▶'}</span>
              <span>🧠 思考中...</span>
              <span className="text-purple-600">{thinkingContent.length} 字</span>
            </button>
            {showThinking && (
              <div className="mt-1 p-3 rounded-lg bg-purple-950/30 border border-purple-900/50 text-xs text-purple-300/80 whitespace-pre-wrap italic max-h-48 overflow-y-auto">
                {thinkingContent}
              </div>
            )}
          </div>
        )}

        {/* Collapsed thinking after completion */}
        {!streaming && thinkingContent && (
          <div className="max-w-[90%] mx-auto">
            <button
              onClick={toggleThinking}
              className="flex items-center gap-2 text-xs text-purple-500 hover:text-purple-400 py-1 w-full text-left"
            >
              <span>{showThinking ? '▼' : '▶'}</span>
              <span>🧠 思考过程</span>
              <span className="text-purple-700">{thinkingContent.length} 字</span>
            </button>
            {showThinking && (
              <div className="mt-1 p-3 rounded-lg bg-purple-950/20 border border-purple-900/30 text-xs text-purple-300/70 whitespace-pre-wrap italic max-h-48 overflow-y-auto">
                {thinkingContent}
              </div>
            )}
          </div>
        )}

        {/* Tool call cards */}
        {toolCalls.map((tc) => (
          <div key={tc.id} className={`max-w-[90%] mx-auto border rounded-lg overflow-hidden text-xs ${
            tc.is_error ? 'border-red-800 bg-red-950/20' :
            tc.status === 'running' ? 'border-yellow-800 bg-yellow-950/20' :
            'border-green-800 bg-green-950/20'
          }`}>
            <div className="flex items-center gap-2 px-3 py-1.5 border-b border-inherit">
              <span>{tc.status === 'running' ? '⏳' : tc.is_error ? '❌' : '✅'}</span>
              <span className="font-mono font-bold text-gray-300">{tc.name}</span>
              <span className="text-gray-500 ml-auto">{tc.status === 'running' ? '执行中...' : '完成'}</span>
            </div>
            {tc.arguments != null && (
              <div className="px-3 py-1 text-gray-500 font-mono break-all">
                {String(JSON.stringify(tc.arguments))}
              </div>
            )}
            {tc.content && (
              <div className={`px-3 py-1.5 whitespace-pre-wrap break-all ${
                tc.is_error ? 'text-red-300' : 'text-gray-400'
              }`}>
                {tc.content.slice(0, 500)}{tc.content.length > 500 ? '...' : ''}
              </div>
            )}
          </div>
        ))}

        {/* Streaming response */}
        {streaming && streamContent && (
          <div className="p-3 rounded-lg max-w-[85%] bg-gray-800 text-sm">
            <p className="whitespace-pre-wrap">
              {streamContent}
              <span className="animate-pulse">▌</span>
            </p>
          </div>
        )}

        <div ref={messagesEndRef} />
      </main>

      {/* Sandbox status bar with permission level dropdown */}
      <div className="px-4 py-1 border-t border-gray-800 bg-gray-950/60 shrink-0 flex items-center gap-4 text-xs text-gray-500">
        <button
          onClick={() => loadSandboxStatus()}
          className="hover:text-gray-300 transition-colors"
          title="点击刷新沙箱状态"
        >
          🖥 {sandboxShell === 'none' ? '...' : sandboxShell}
        </button>

        {/* Permission level dropdown */}
        <div className="relative">
          <button
            onClick={() => setShowLevelDropdown(!showLevelDropdown)}
            onBlur={() => setTimeout(() => setShowLevelDropdown(false), 150)}
            className="hover:text-yellow-400 transition-colors flex items-center gap-1"
            title="切换权限模式"
          >
            🔒 {sandboxLevel} ▾
          </button>
          {showLevelDropdown && (
            <div className="absolute bottom-full left-0 mb-1 bg-gray-800 border border-gray-700 rounded-lg shadow-xl py-1 z-50 min-w-[120px]">
              {availableLevels.map((lv) => (
                <button
                  key={lv.key}
                  onClick={() => {
                    setPermissionLevel(lv.key);
                    setShowLevelDropdown(false);
                  }}
                  className={`block w-full text-left px-3 py-1.5 text-xs hover:bg-gray-700 transition-colors ${
                    lv.key === sandboxPermissionKey
                      ? 'text-yellow-400 font-bold'
                      : 'text-gray-400'
                  }`}
                >
                  {lv.key === sandboxPermissionKey ? '● ' : '  '}
                  {lv.label}
                </button>
              ))}
            </div>
          )}
        </div>

        <span>📦 {activeSessions} sessions</span>
      </div>

      {/* Input */}
      <footer className="p-3 border-t border-gray-800 bg-gray-950/80 shrink-0">
        <div className="flex gap-2 max-w-3xl mx-auto">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSend()}
            placeholder={streaming ? '回复中...' : '输入消息...'}
            disabled={streaming}
            className="flex-1 bg-gray-800 border border-gray-700 rounded-lg px-4 py-2 text-sm focus:outline-none focus:border-blue-500 disabled:opacity-50"
          />
          <button
            onClick={handleSend}
            disabled={streaming || !input.trim()}
            className="bg-blue-600 hover:bg-blue-500 px-4 py-2 rounded-lg text-sm font-medium transition-colors disabled:opacity-50 shrink-0"
          >
            {streaming ? '...' : '发送'}
          </button>
        </div>
      </footer>
    </div>
  );
}
