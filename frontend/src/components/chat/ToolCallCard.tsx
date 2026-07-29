import { useState } from 'react';

interface TcProps {
  tc: {
    id: string;
    name: string;
    arguments?: unknown;
    content?: string;
    is_error?: boolean;
    status: 'running' | 'done';
  };
}

function fmtArgs(args: unknown): string {
  if (!args || typeof args !== 'object') return '';
  const entries = Object.entries(args as Record<string, unknown>);
  if (entries.length === 0) return '';
  return entries.map(([k, v]) => {
    const val = typeof v === 'string' ? (v.length > 80 ? v.slice(0, 80) + '…' : v) : JSON.stringify(v);
    return `${k}: ${val}`;
  }).join(', ');
}

export default function ToolCallCard({ tc }: TcProps) {
  const [expanded, setExpanded] = useState(false);
  const argsStr = fmtArgs(tc.arguments);

  return (
    <div className="my-0.5">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1.5 text-xs font-mono text-muted-foreground hover:text-foreground transition-colors w-full text-left"
      >
        <span className="shrink-0">{expanded ? '▾' : '▸'}</span>
        <span>{tc.status === 'running' ? '⏳' : tc.is_error ? '✗' : '✓'}</span>
        <span>tool:{tc.name}</span>
        {argsStr && !expanded && (
          <span className="text-muted-foreground/60 truncate">({argsStr})</span>
        )}
      </button>
      {expanded && (
        <div className="mt-1 ml-4 pl-3 border-l-2 border-muted">
          {argsStr && (
            <div className="text-xs text-muted-foreground/60 mb-1">({argsStr})</div>
          )}
          {tc.content && (
            <div className={`text-xs whitespace-pre-wrap break-all ${tc.is_error ? 'text-destructive' : 'text-muted-foreground'}`}>
              {tc.content.length > 4000 ? tc.content.slice(0, 4000) + '…' : tc.content}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
