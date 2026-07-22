// Hermes pattern: floating "back to bottom" button when user scrolls away.
// Only shown when new messages are arriving and user is not following.

interface BackToBottomProps {
  visible: boolean;
  onClick: () => void;
}

export default function BackToBottom({ visible, onClick }: BackToBottomProps) {
  if (!visible) return null;

  return (
    <button
      onClick={onClick}
      className="absolute bottom-4 right-6 z-10 w-9 h-9 rounded-full bg-primary text-primary-foreground
                 shadow-lg flex items-center justify-center text-sm font-bold
                 hover:bg-primary/90 transition-all animate-in fade-in slide-in-from-bottom-2"
      title="回到底部"
    >
      ↓
    </button>
  );
}
