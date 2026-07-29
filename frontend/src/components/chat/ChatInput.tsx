import { useRef, useState, useEffect, useCallback, type KeyboardEvent } from 'react';
import { useStore } from '../../store';
import CommandPicker from './CommandPicker';

interface ChatInputProps {
  onSend: (text: string) => void;
  disabled: boolean;
}

export default function ChatInput({ onSend, disabled }: ChatInputProps) {
  const [text, setText] = useState('');
  const [pickerVisible, setPickerVisible] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const streaming = useStore((s) => s.streaming);
  const abortStream = useStore((s) => s.abortStream);

  // Show picker when text starts with /, hide when it doesn't
  useEffect(() => {
    setPickerVisible(text.trimStart().startsWith('/'));
  }, [text]);

  // Auto-height: cap at 25% viewport
  useEffect(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = 'auto';
    const maxH = window.innerHeight * 0.25;
    ta.style.height = Math.min(ta.scrollHeight, maxH) + 'px';
  }, [text]);

  const handleCommandSelect = useCallback(
    (command: string) => {
      setText(command + ' ');
      setPickerVisible(false);
      taRef.current?.focus();
    },
    [],
  );

  const handleCommandDismiss = useCallback(() => {
    setPickerVisible(false);
  }, []);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      // If picker is visible, delegate arrow/enter/escape/tab to it
      if (pickerVisible) {
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Tab') {
          return; // let CommandPicker handle via document listener
        }
        if (e.key === 'Enter' && !e.shiftKey) {
          return; // let CommandPicker select
        }
        if (e.key === 'Escape') {
          e.preventDefault();
          setPickerVisible(false);
          return;
        }
      }

      if (e.key === 'Escape' && streaming) {
        e.preventDefault();
        abortStream();
        return;
      }
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        if (streaming) { abortStream(); return; }
        if (!text.trim() || disabled) return;
        onSend(text.trim());
        setText('');
        setPickerVisible(false);
        if (taRef.current) taRef.current.style.height = 'auto';
      }
    },
    [text, disabled, onSend, streaming, abortStream, pickerVisible],
  );

  const doSend = () => {
    if (!text.trim() || disabled) return;
    onSend(text.trim());
    setText('');
    setPickerVisible(false);
    if (taRef.current) taRef.current.style.height = 'auto';
  };

  return (
    <footer className="p-3 bg-background shrink-0">
      <div className="max-w-3xl mx-auto flex flex-col gap-2 relative">
        {/* Command picker — positioned above the textarea */}
        {pickerVisible && !disabled && (
          <CommandPicker
            text={text}
            onSelect={handleCommandSelect}
            onDismiss={handleCommandDismiss}
          />
        )}

        {/* Textarea — full width, no scrollbar */}
        <textarea
          ref={taRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={disabled ? '回复中...' : '输入消息... (Enter 发送, Shift+Enter 换行, / 命令)'}
          disabled={disabled}
          rows={1}
          className="w-full bg-secondary border border-border rounded-lg px-4 py-2.5 text-sm
                     text-foreground placeholder:text-muted-foreground resize-none
                     focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary
                     disabled:opacity-50 transition-colors leading-relaxed
                     [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
          style={{ maxHeight: '25vh' }}
        />

        {/* Send / Stop button */}
        {streaming ? (
          <button
            onClick={abortStream}
            className="self-end bg-destructive hover:bg-destructive/90 text-destructive-foreground px-4 py-2 rounded-lg
                       text-sm font-medium transition-colors
                       shadow-[inset_0_1px_0_0_rgba(255,255,255,0.15),inset_0_-1px_0_0_rgba(0,0,0,0.1)]
                       active:shadow-[inset_0_1px_0_0_rgba(0,0,0,0.1),inset_0_-1px_0_0_rgba(255,255,255,0.08)]"
          >
            停止
          </button>
        ) : (
          <button
            onClick={doSend}
            disabled={!text.trim()}
            className="self-end bg-primary hover:bg-primary/90 text-primary-foreground px-4 py-2 rounded-lg
                       text-sm font-medium transition-colors disabled:opacity-50
                       shadow-[inset_0_1px_0_0_rgba(255,255,255,0.15),inset_0_-1px_0_0_rgba(0,0,0,0.1)]
                       active:shadow-[inset_0_1px_0_0_rgba(0,0,0,0.1),inset_0_-1px_0_0_rgba(255,255,255,0.08)]"
          >
            发送
          </button>
        )}
      </div>
    </footer>
  );
}
