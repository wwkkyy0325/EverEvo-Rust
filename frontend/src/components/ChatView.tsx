import { useCallback } from 'react';
import { useStore, type MessageItem } from '../store';
import { useAutoScroll } from '../hooks/useAutoScroll';
import ChatBubble from './chat/ChatBubble';
import ToolCallCard from './chat/ToolCallCard';
import ThinkingChunk from './chat/ThinkingChunk';
import TodoPanel from './TodoPanel';
import SubAgentPanel from './SubAgentPanel';
import MemoryPanel from './MemoryPanel';
import AuditPanel from './AuditPanel';
import ChatInput from './chat/ChatInput';
import BackToBottom from './chat/BackToBottom';

export default function ChatView() {
  const {
    activeSessionId, sessions,
    messages, messagesLoading, hasMoreHistory,
    streaming, draftId,
    loadMoreMessages, sendMessage,
  } = useStore();

  const activeSession = sessions.find((s) => s.id === activeSessionId);

  const { containerRef, showBackToBottom, onScroll, scrollToBottom } = useAutoScroll([messages]);

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
        requestAnimationFrame(() => { if (el) el.scrollTop = el.scrollHeight - prevHeight; });
      });
    }
  }, [hasMoreHistory, messagesLoading, loadMoreMessages, onScroll, containerRef]);

  const handleSend = useCallback((text: string) => {
    if (!text.trim() || streaming) return;
    sendMessage(text.trim());
  }, [streaming, sendMessage]);

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
      <header className="px-4 py-3 bg-background shrink-0 flex items-center justify-between">
        <div>
          <span className="text-sm font-semibold text-foreground truncate">{activeSession.title}</span>
          <p className="text-xs text-muted-foreground">{activeSession.message_count} 轮对话</p>
        </div>
        <div className="flex gap-1">
          <button onClick={() => useStore.setState((s) => ({ showMemory: !s.showMemory }))}
            className="text-xs text-muted-foreground hover:text-foreground transition-colors px-2 py-1 rounded border border-border/50">🧠</button>
          <button onClick={() => useStore.setState((s) => ({ showAudit: !s.showAudit }))}
            className="text-xs text-muted-foreground hover:text-foreground transition-colors px-2 py-1 rounded border border-border/50">📋</button>
        </div>
      </header>

      <TodoPanel />
      <SubAgentPanel />

      <main ref={containerRef} onScroll={handleScroll}
        className="flex-1 overflow-y-auto px-4 py-3 space-y-3 relative
                     [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden">
        {messagesLoading && <div className="flex justify-center py-2"><div className="animate-spin w-4 h-4 border-2 border-primary border-t-transparent rounded-full" /></div>}
        {hasMoreHistory && !messagesLoading && <p className="text-xs text-muted-foreground text-center py-1">向上滚动加载更多</p>}
        {!messagesLoading && messages.length === 0 && !streaming && (
          <div className="text-center mt-12">
            <p className="text-2xl mb-2">🤖</p>
            <p className="text-sm text-foreground/70 mb-1">开始对话</p>
            <p className="text-xs text-muted-foreground/50">
              Enter 发送 · Shift+Enter 换行 · Esc 停止
            </p>
          </div>
        )}

        {/* ALL messages (including streaming draft) — ChatBubble handles blocks internally */}
        {mergeHistory(messages).map((item: any) => {
          if (item._type === 'tool-group') return (
            <div key={item.id}>
              {item.thinking && <ThinkingChunk content={item.thinking} />}
              {item.tools.map((tc: any) => <ToolCallCard key={tc.id} tc={tc} />)}
            </div>
          );
          // Pass draftId so ChatBubble knows to show cursor for the streaming draft
          return <ChatBubble key={item.id} msg={item} isStreaming={item.id === draftId} />;
        })}

        <BackToBottom visible={showBackToBottom} onClick={() => scrollToBottom(true)} />
      </main>

      <ChatInput onSend={handleSend} disabled={streaming} />
      <MemoryPanel />
      <AuditPanel />
    </div>
  );
}

// ── Merge DB tool-stub messages with their tool-result messages ────

function mergeHistory(msgs: MessageItem[]): any[] {
  const out: any[] = [];
  let i = 0;
  while (i < msgs.length) {
    const m = msgs[i];
    // Messages with blocks are self-contained — render as-is
    if (m.blocks && m.blocks.length > 0) { out.push(m); i++; continue; }
    // Tool-result messages consumed by the tool-group merge below — skip
    if (m.role === 'tool') { i++; continue; }
    // Legacy tool-stub: has tool_calls but empty content
    if (m.role === 'assistant' && m.tool_calls && !m.content) {
      const tcs = Array.isArray(m.tool_calls) ? m.tool_calls as any[] : [m.tool_calls];
      const tools: any[] = [];
      let ri = i + 1;
      for (let ti = 0; ti < tcs.length; ti++) {
        const tc = tcs[ti];
        const result = (ri < msgs.length && msgs[ri]?.role === 'tool') ? msgs[ri].content : undefined;
        tools.push({ id: tc.id || `${m.id}-${ti}`, name: tc.name || 'tool', arguments: tc.arguments, status: 'done' as const, content: result });
        if (ri < msgs.length && msgs[ri]?.role === 'tool') ri++;
      }
      out.push({ _type: 'tool-group', id: m.id, thinking: m.thinking, tools });
      i = ri;
    } else { out.push(m); i++; }
  }
  return out;
}
