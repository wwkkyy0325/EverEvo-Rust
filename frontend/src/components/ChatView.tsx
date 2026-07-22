import { useCallback } from 'react';
import { useStore } from '../store';
import { useAutoScroll } from '../hooks/useAutoScroll';
import ChatBubble from './chat/ChatBubble';
import ToolCallCard from './chat/ToolCallCard';
import ThinkingPanel from './chat/ThinkingPanel';
import ChatInput from './chat/ChatInput';
import BackToBottom from './chat/BackToBottom';

type ConnState = 'idle' | 'connecting' | 'live' | 'error';

export default function ChatView() {
  const {
    activeSessionId, sessions,
    messages, messagesLoading, hasMoreHistory,
    streaming, streamContent, thinkingContent, showThinking, toolCalls,
    loadMoreMessages, sendMessage, loadSandboxStatus, loadAudit,
    toggleMemory, toggleDomain,
  } = useStore();

  const toggleThinking = () => useStore.setState((s) => ({ showThinking: !s.showThinking }));
  const activeSession = sessions.find((s) => s.id === activeSessionId);

  // ── Smart auto-scroll (Hermes pattern) ──────────────────────────
  const { containerRef, showBackToBottom, onScroll, scrollToBottom } = useAutoScroll([
    messages, streamContent, thinkingContent, toolCalls,
  ]);

  // ── Infinite scroll — load older messages when near top ─────────
  const loadingRef = { current: false };
  const handleScroll = useCallback(() => {
    onScroll();
    const el = containerRef.current;
    if (!el || loadingRef.current) return;
    if (el.scrollTop < 60 && hasMoreHistory && !messagesLoading) {
      loadingRef.current = true;
      const prevHeight = el.scrollHeight;
      loadMoreMessages().finally(() => {
        loadingRef.current = false;
        requestAnimationFrame(() => {
          if (el) el.scrollTop = el.scrollHeight - prevHeight;
        });
      });
    }
  }, [hasMoreHistory, messagesLoading, loadMoreMessages, onScroll, containerRef]);

  // ── Send handler ────────────────────────────────────────────────
  const handleSend = useCallback((text: string) => {
    if (!text.trim() || streaming) return;
    sendMessage(text.trim());
  }, [streaming, sendMessage]);

  // ── Connection state ────────────────────────────────────────────
  const connection: ConnState = useStore((s) => {
    if (s.streaming) return 'live';
    return 'live';
  });

  // ── Empty state ─────────────────────────────────────────────────
  if (!activeSession) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-center">
          <p className="text-muted-foreground text-lg mb-2">EverEvo</p>
          <p className="text-muted-foreground/60 text-sm">选择或创建对话开始</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
      {/* Header */}
      <header className="px-4 py-3 bg-background shrink-0 flex items-center justify-between">
        <div>
          <h1 className="text-sm font-semibold text-foreground truncate">{activeSession.title}</h1>
          <p className="text-xs text-muted-foreground">{activeSession.message_count} 条消息</p>
        </div>
        <div className="flex items-center gap-2">
          <button onClick={() => { activeSessionId && loadAudit(activeSessionId); }}
            className="text-xs text-muted-foreground hover:text-thinking hover:border-thinking/50 px-2 py-1 rounded border border-border transition-colors">审计</button>
          <button onClick={toggleMemory}
            className="text-xs text-muted-foreground hover:text-warning hover:border-warning/50 px-2 py-1 rounded border border-border transition-colors">🧠 记忆</button>
          <button onClick={toggleDomain}
            className="text-xs text-muted-foreground hover:text-success hover:border-success/50 px-2 py-1 rounded border border-border transition-colors">📚 领域</button>
        </div>
      </header>

      {/* Messages */}
      <main ref={containerRef} onScroll={handleScroll}
        className="flex-1 overflow-y-auto px-4 py-3 space-y-3 relative">
        {messagesLoading && (
          <div className="flex justify-center py-2">
            <div className="animate-spin w-4 h-4 border-2 border-primary border-t-transparent rounded-full" />
          </div>
        )}
        {hasMoreHistory && !messagesLoading && (
          <p className="text-xs text-muted-foreground text-center py-1">向上滚动加载更多</p>
        )}
        {messages.length === 0 && !streaming && (
          <p className="text-muted-foreground text-center mt-8 text-sm">发送消息开始对话</p>
        )}

        {messages.map((msg) => <ChatBubble key={msg.id} msg={msg} />)}

        <ThinkingPanel content={thinkingContent} streaming={streaming} showThinking={showThinking} onToggle={toggleThinking} />
        {toolCalls.map((tc) => <ToolCallCard key={tc.id} tc={tc} />)}

        {streaming && streamContent && (
          <div className="p-3 rounded-lg max-w-[85%] bg-chat-assistant text-chat-assistant-foreground text-sm">
            <p className="whitespace-pre-wrap">{streamContent}<span className="animate-pulse">▌</span></p>
          </div>
        )}

        {/* Back to bottom floating button (Hermes pattern) */}
        <BackToBottom visible={showBackToBottom} onClick={() => scrollToBottom(true)} />
      </main>

      {/* Input */}
      <ChatInput onSend={handleSend} disabled={streaming} connection={connection} />
    </div>
  );
}
