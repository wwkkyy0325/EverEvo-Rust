import { useState, useEffect, useRef, useCallback } from 'react';

interface CommandDef {
  name: string;
  description: string;
  display: string;
}

interface CommandPickerProps {
  /** Current text in the input (to detect / and filter). */
  text: string;
  /** Called when user selects a command — fills the input with the command. */
  onSelect: (command: string) => void;
  /** Called on Escape to dismiss. */
  onDismiss: () => void;
}

export default function CommandPicker({ text, onSelect, onDismiss }: CommandPickerProps) {
  const [commands, setCommands] = useState<CommandDef[]>([]);
  const [selectedIdx, setSelectedIdx] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  // Load commands once on mount
  useEffect(() => {
    fetch('/api/commands')
      .then((r) => r.json())
      .then((data) => setCommands(data.commands ?? []))
      .catch(() => setCommands([]));
  }, []);

  // Filter by typed text after the /
  const query = text.startsWith('/') ? text.slice(1).toLowerCase() : '';
  const filtered = commands.filter(
    (c) => c.name.includes(query) || c.description.toLowerCase().includes(query),
  );

  // Reset selection when filtered list changes
  useEffect(() => {
    setSelectedIdx(0);
  }, [query]);

  // Scroll selected into view
  useEffect(() => {
    const el = listRef.current?.children[selectedIdx] as HTMLElement | undefined;
    el?.scrollIntoView({ block: 'nearest' });
  }, [selectedIdx]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSelectedIdx((i) => (i + 1) % Math.max(filtered.length, 1));
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSelectedIdx((i) => (i - 1 + filtered.length) % Math.max(filtered.length, 1));
      } else if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault();
        if (filtered[selectedIdx]) {
          onSelect(filtered[selectedIdx].display);
        }
      } else if (e.key === 'Escape') {
        e.preventDefault();
        onDismiss();
      }
    },
    [filtered, selectedIdx, onSelect, onDismiss],
  );

  // Attach global keydown when visible
  useEffect(() => {
    document.addEventListener('keydown', handleKeyDown as any);
    return () => document.removeEventListener('keydown', handleKeyDown as any);
  }, [handleKeyDown]);

  if (!text.startsWith('/') || filtered.length === 0) return null;

  return (
    <div
      ref={listRef}
      className="absolute bottom-full left-0 right-0 mb-1 bg-secondary border border-border
                 rounded-lg shadow-lg max-h-48 overflow-y-auto z-50
                 [scrollbar-width:thin] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
    >
      {filtered.map((cmd, i) => (
        <button
          key={cmd.name}
          className={`w-full text-left px-3 py-2 text-sm flex items-center gap-2
            transition-colors cursor-pointer
            ${i === selectedIdx
              ? 'bg-primary/10 text-foreground'
              : 'text-foreground hover:bg-secondary/80'
            }`}
          onMouseDown={(e) => {
            e.preventDefault();
            onSelect(cmd.display);
          }}
          onMouseEnter={() => setSelectedIdx(i)}
        >
          <span className="font-mono text-xs text-primary/70 min-w-fit">{cmd.display}</span>
          <span className="text-muted-foreground truncate">{cmd.description}</span>
        </button>
      ))}
    </div>
  );
}
