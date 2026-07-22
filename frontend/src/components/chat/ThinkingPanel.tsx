interface ThinkingPanelProps {
  content: string;
  streaming: boolean;
  showThinking: boolean;
  onToggle: () => void;
}

export default function ThinkingPanel({ content, streaming, showThinking, onToggle }: ThinkingPanelProps) {
  if (!content) return null;

  const isStreaming = streaming && showThinking;
  const isDone = !streaming;

  return (
    <div className="max-w-[90%] mx-auto">
      <button
        onClick={onToggle}
        className={`flex items-center gap-2 text-xs py-1 w-full text-left transition-colors ${
          isStreaming
            ? 'text-thinking hover:text-thinking/80'
            : 'text-thinking/70 hover:text-thinking/60'
        }`}
      >
        <span>{showThinking ? '▼' : '▶'}</span>
        <span>🧠 {streaming ? '思考中...' : '思考过程'}</span>
        <span className="text-thinking/50 text-[10px]">{content.length} 字</span>
      </button>

      {showThinking && (
        <div className={`mt-1 p-3 rounded-lg border text-xs whitespace-pre-wrap italic max-h-48 overflow-y-auto ${
          isStreaming
            ? 'bg-thinking/10 border-thinking/30 text-thinking/90'
            : 'bg-thinking/5 border-thinking/20 text-thinking/70'
        }`}>
          {content}
        </div>
      )}
    </div>
  );
}
