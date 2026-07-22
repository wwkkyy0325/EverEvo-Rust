// Hermes pattern: auto-height textarea with send/stop, connection status, pre-flight.
//
// Behaviours:
//   - Enter → send (unless Shift is held)
//   - Shift+Enter → newline
//   - Auto-height: grows with content up to 24% viewport, then scrolls
//   - Connection dot: green=live, yellow=connecting, red=error, gray=idle

import { useRef, useState, useEffect, useCallback, type KeyboardEvent } from 'react';

type ConnectionState = 'idle' | 'connecting' | 'live' | 'error';

interface ChatInputProps {
  onSend: (text: string) => void;
  disabled: boolean;        // streaming = disabled
  connection: ConnectionState;
}

export default function ChatInput({ onSend, disabled, connection }: ChatInputProps) {
  const [text, setText] = useState('');
  const taRef = useRef<HTMLTextAreaElement>(null);

  // Auto-height: cap at 24% viewport
  useEffect(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = 'auto';
    const maxH = window.innerHeight * 0.24;
    ta.style.height = Math.min(ta.scrollHeight, maxH) + 'px';
  }, [text]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        if (!text.trim() || disabled) return;
        onSend(text.trim());
        setText('');
        // Reset height
        if (taRef.current) taRef.current.style.height = 'auto';
      }
      // Shift+Enter → default behaviour (newline)
    },
    [text, disabled, onSend],
  );

  return (
    <footer className="p-3 bg-background shrink-0">
      <div className="flex gap-2 max-w-3xl mx-auto items-end">
        {/* Textarea */}
        <div className="flex-1 relative">
          <textarea
            ref={taRef}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={disabled ? '回复中...' : '输入消息... (Enter 发送, Shift+Enter 换行)'}
            disabled={disabled}
            rows={1}
            className="w-full bg-secondary border border-border rounded-lg px-4 py-2.5 text-sm
                       text-foreground placeholder:text-muted-foreground resize-none
                       focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary
                       disabled:opacity-50 transition-colors leading-relaxed"
          />
        </div>

        {/* Connection dot + Send */}
        <div className="flex items-center gap-2 shrink-0">
          <ConnectionDot state={connection} />
          <button
            onClick={() => {
              if (!text.trim() || disabled) return;
              onSend(text.trim());
              setText('');
              if (taRef.current) taRef.current.style.height = 'auto';
            }}
            disabled={disabled || !text.trim()}
            className="bg-primary hover:bg-primary/90 text-primary-foreground px-4 py-2.5 rounded-lg
                       text-sm font-medium transition-colors disabled:opacity-50 shrink-0
                       shadow-[inset_0_1px_0_0_rgba(255,255,255,0.15),inset_0_-1px_0_0_rgba(0,0,0,0.1)]
                       active:shadow-[inset_0_1px_0_0_rgba(0,0,0,0.1),inset_0_-1px_0_0_rgba(255,255,255,0.08)]"
          >
            {disabled ? '...' : '发送'}
          </button>
        </div>
      </div>
    </footer>
  );
}

// ── Connection dot ──────────────────────────────────────────────────

function ConnectionDot({ state }: { state: ConnectionState }) {
  const colors: Record<ConnectionState, string> = {
    idle:    'bg-muted-foreground/40',
    connecting: 'bg-warning animate-pulse',
    live:    'bg-success',
    error:   'bg-destructive',
  };
  const labels: Record<ConnectionState, string> = {
    idle:    '待机',
    connecting: '连接中',
    live:    '在线',
    error:   '断开',
  };

  return (
    <span
      className={`inline-block w-2 h-2 rounded-full shrink-0 ${colors[state]}`}
      title={labels[state]}
    />
  );
}
