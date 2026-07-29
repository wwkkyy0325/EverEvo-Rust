import { useState } from 'react';
import MarkdownContent from './MarkdownContent';

/**
 * Claude Code-style thinking display.
 *
 * - Default: collapsed "∴ 思考中…" / "∴ 思考过程" label (click to expand)
 * - Expanded: full thinking content rendered as MARKDOWN (not plain text)
 */
export default function ThinkingChunk({ content, isLive }: { content: string; isLive?: boolean }) {
  const [show, setShow] = useState(false);

  return (
    <div>
      <button
        onClick={() => setShow(!show)}
        className={`flex items-center gap-2 text-xs py-1 w-full text-left transition-colors ${
          isLive ? 'text-muted-foreground hover:text-foreground' : 'text-muted-foreground/60 hover:text-muted-foreground'
        }`}
      >
        <span className="shrink-0">{show ? '▼' : '▶'}</span>
        <span className="italic">{isLive ? '∴ 思考中…' : '∴ 思考过程'}</span>
        {!isLive && <span className="text-muted-foreground/40 text-[10px]">{content.length} 字</span>}
        {isLive && <span className="inline-block w-1.5 h-1.5 rounded-full bg-warning animate-pulse" />}
      </button>
      {show && (
        <div className="mt-1 ml-5 pl-3 border-l-2 border-muted/50 text-xs text-muted-foreground/80 max-h-64 overflow-y-auto">
          <MarkdownContent>{content}</MarkdownContent>
        </div>
      )}
    </div>
  );
}
