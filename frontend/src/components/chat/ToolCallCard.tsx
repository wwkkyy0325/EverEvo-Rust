import { useState } from 'react';
import type { ToolCallEvent } from '../../store';

interface ToolCallCardProps {
  tc: ToolCallEvent;
}

const TOOL_COLORS: Record<string, { border: string; bg: string }> = {
  shell:  { border: 'border-tool-shell/50',  bg: 'bg-tool-shell/10' },
  web_search: { border: 'border-tool-web/50', bg: 'bg-tool-web/10' },
  web_fetch:  { border: 'border-tool-web/50', bg: 'bg-tool-web/10' },
  file_read:  { border: 'border-tool-file/50', bg: 'bg-tool-file/10' },
  file_write: { border: 'border-tool-file/50', bg: 'bg-tool-file/10' },
  code_exec:  { border: 'border-tool-code/50', bg: 'bg-tool-code/10' },
};

function getToolColors(name: string) {
  return TOOL_COLORS[name] ?? { border: 'border-border', bg: 'bg-secondary/30' };
}

export default function ToolCallCard({ tc }: ToolCallCardProps) {
  const [expanded, setExpanded] = useState(false);
  const colors = getToolColors(tc.name);

  return (
    <div className={`max-w-[90%] mx-auto border rounded-lg overflow-hidden text-xs ${colors.border} ${colors.bg}`}>
      {/* Header — always visible */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:opacity-80 transition-opacity"
      >
        <span>{tc.status === 'running' ? '⏳' : tc.is_error ? '❌' : '✅'}</span>
        <span className="font-mono font-bold text-foreground">{tc.name}</span>
        <span className="text-muted-foreground ml-auto text-[10px]">
          {tc.status === 'running' ? '执行中...' : expanded ? '收起' : '展开'}
        </span>
      </button>

      {/* Expandable body */}
      {expanded && (
        <>
          {tc.arguments != null && (
            <div className="px-3 py-1 text-muted-foreground font-mono break-all border-t border-inherit">
              {JSON.stringify(tc.arguments, null, 2)}
            </div>
          )}
          {tc.content && (
            <div className={`px-3 py-1.5 whitespace-pre-wrap break-all border-t border-inherit ${
              tc.is_error ? 'text-destructive' : 'text-muted-foreground'
            }`}>
              {tc.content.slice(0, 500)}{tc.content.length > 500 ? '...' : ''}
            </div>
          )}
        </>
      )}
    </div>
  );
}
