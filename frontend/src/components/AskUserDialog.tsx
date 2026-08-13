// Ask-user dialog — the agent asked a question (via the `ask_user` tool) and is
// blocked waiting for a free-text reply. Mirrors ConfirmDialog, but accepts a
// free-text answer instead of Allow/Deny. Multiple asks stack in a queue.

import { useState } from 'react';
import { useStore } from '../store';

export default function AskUserDialog() {
  const askQueue = useStore((s) => s.askQueue);
  const resolveAsk = useStore((s) => s.resolveAsk);
  const [reply, setReply] = useState('');

  const ask = askQueue[0] ?? null;
  if (!ask) return null;

  const queueLen = askQueue.length;

  const submit = () => {
    const text = reply.trim();
    if (!text) return;
    setReply('');
    void resolveAsk(text);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
      <div className="bg-background border border-accent/50 rounded-xl shadow-2xl max-w-lg w-full flex flex-col">
        {/* Header */}
        <div className="flex items-start gap-3 px-5 pt-5 pb-2 shrink-0">
          <span className="text-2xl shrink-0">❓</span>
          <div className="min-w-0 min-h-0">
            <span className="text-sm font-bold text-accent">问题{queueLen > 1 ? ` (${queueLen} 个待处理)` : ''}</span>
            <p className="text-xs text-muted-foreground mt-1 max-h-24 overflow-y-auto leading-relaxed whitespace-pre-wrap">{ask.question}</p>
          </div>
        </div>

        {/* Reply input */}
        <div className="px-5 pb-3">
          <textarea
            value={reply}
            onChange={(e) => setReply(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); submit(); }
            }}
            placeholder="输入你的回复…"
            className="w-full bg-secondary rounded px-3 py-2 text-sm text-foreground resize-none focus:outline-none focus:ring-1 focus:ring-accent min-h-16"
            autoFocus
          />
        </div>

        {/* Hint + submit */}
        <div className="flex items-center justify-between gap-2 px-5 pb-5 pt-2 border-t border-border shrink-0">
          <p className="text-xs text-muted-foreground">Agent 正在等待你的回复(可取消 SSE 中断)。</p>
          <button
            onClick={submit}
            disabled={!reply.trim()}
            className="px-4 py-2 rounded text-sm bg-accent hover:bg-accent/90 text-accent-foreground font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            提交
          </button>
        </div>
      </div>
    </div>
  );
}
