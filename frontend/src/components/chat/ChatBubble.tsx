import type { MessageItem } from '../../store';

interface ChatBubbleProps {
  msg: MessageItem;
}

export default function ChatBubble({ msg }: ChatBubbleProps) {
  const isUser = msg.role === 'user';

  return (
    <div
      className={`p-3 rounded-lg max-w-[85%] text-sm ${
        isUser
          ? 'bg-chat-user/40 text-chat-user-foreground ml-auto'
          : 'bg-chat-assistant text-chat-assistant-foreground'
      }`}
    >
      <p className="whitespace-pre-wrap">{msg.content}</p>
    </div>
  );
}
