import MarkdownContent from './MarkdownContent';
import ToolCallCard from './ToolCallCard';
import ThinkingChunk from './ThinkingChunk';
import MessageTimestamp from '../MessageTimestamp';
import type { MessageItem, ContentBlock } from '../../store';

export default function ChatBubble({ msg, isStreaming }: { msg: MessageItem; isStreaming?: boolean }) {
  if (msg.role === 'tool') return null;

  const liveIdx = isStreaming ? (msg.activeBlockIdx ?? -1) : -1;

  if (msg.role === 'user') {
    return (
      <div className="max-w-[85%] ml-auto">
        <div className="p-3 rounded-lg text-sm bg-chat-user/40 text-chat-user-foreground">
          <p className="whitespace-pre-wrap">{msg.content}</p>
        </div>
        <div className="text-right mt-0.5"><MessageTimestamp createdAt={msg.created_at} /></div>
      </div>
    );
  }

  // ── Content blocks (new format) — render in order ──────────────
  if (msg.blocks && msg.blocks.length > 0) {
    return (
      <div className="space-y-1">
        {msg.blocks.map((block: ContentBlock) => (
          <RenderBlock key={block.index} block={block} isLive={isStreaming && block.index === liveIdx} />
        ))}
        {isStreaming && <span className="animate-pulse text-foreground text-sm">▌</span>}
        {!isStreaming && <MessageTimestamp createdAt={msg.created_at} />}
      </div>
    );
  }

  // ── Legacy format — thinking + tool_calls + markdown ────────────
  return (
    <div>
      {msg.thinking && <ThinkingChunk content={msg.thinking} />}
      {msg.tool_calls ? <ToolCallsInline raw={msg.tool_calls} /> : null}
      {msg.content && <MarkdownContent>{msg.content}</MarkdownContent>}
      <MessageTimestamp createdAt={msg.created_at} />
    </div>
  );
}

// ── Render a single block (used by both ChatBubble and ChatView streaming) ─

function RenderBlock({ block, isLive }: { block: ContentBlock; isLive?: boolean }) {
  switch (block.type) {
    case 'thinking':
      return block.thinking ? <ThinkingChunk content={block.thinking} isLive={isLive} /> : null;
    case 'tool_use': {
      let args: unknown = undefined;
      if (block.toolInput) {
        try { args = JSON.parse(block.toolInput); } catch { /* ignore */ }
      }
      return (
        <ToolCallCard
          tc={{
            id: block.toolId || '',
            name: block.toolName || 'tool',
            arguments: args,
            content: block.toolResult,
            is_error: block.toolError || false,
            status: block.toolResult != null ? 'done' as const : 'running' as const,
          }}
        />
      );
    }
    case 'text':
      return block.text ? <MarkdownContent>{block.text}</MarkdownContent> : null;
    default:
      return null;
  }
}

// ── Legacy helpers ───────────────────────────────────────────────────

function ToolCallsInline({ raw }: { raw: unknown }) {
  const tcs = normalizeTools(raw);
  if (tcs.length === 0) return null;

  return (
    <>
      {tcs.map((tc: any, i: number) => (
        <ToolCallCard
          key={tc.id || `tc-${i}`}
          tc={{
            id: tc.id || `tc-${i}`,
            name: tc.name || 'tool',
            arguments: tc.arguments,
            status: 'done' as const,
            content: undefined,
          }}
        />
      ))}
    </>
  );
}

function normalizeTools(raw: unknown): any[] {
  if (!raw) return [];
  if (Array.isArray(raw)) return raw;
  if (typeof raw === 'string') {
    try { return normalizeTools(JSON.parse(raw)); } catch { return []; }
  }
  if (typeof raw === 'object') return [raw];
  return [];
}
