import { useStore } from '../store';

/**
 * Claude Code-style TodoWrite panel.
 *
 * Shows the current session's task list with a progress bar and
 * per-task status (pending / in_progress / completed).
 * Only visible when there are active (non-completed) todos.
 */
export default function TodoPanel() {
  const todos = useStore((s) => s.todos);

  if (!todos || todos.length === 0) return null;

  const completed = todos.filter((t) => t.status === 'completed').length;
  const total = todos.length;
  const pct = total > 0 ? Math.round((completed / total) * 100) : 0;
  const allDone = completed === total;

  return (
    <div className="px-4 py-2">
      <div className="rounded-lg border border-border bg-muted/30 overflow-hidden">
        {/* Header with progress */}
        <div className="flex items-center gap-2 px-3 py-2 text-xs">
          <span className="font-medium text-foreground/80">
            {allDone ? '全部完成' : '任务列表'}
          </span>
          <span className="text-muted-foreground font-mono text-[10px]">
            {completed}/{total}
          </span>
          <div className="flex-1 h-1 rounded-full bg-muted overflow-hidden">
            <div
              className="h-full rounded-full bg-primary/60 transition-all duration-500"
              style={{ width: `${pct}%` }}
            />
          </div>
          <span className="text-muted-foreground font-mono text-[10px]">{pct}%</span>
        </div>

        {/* Task list */}
        <div className="border-t border-border px-3 py-1.5 space-y-0.5">
          {todos.map((t, i) => (
            <div key={i} className="flex items-center gap-2 text-xs">
              <span className="shrink-0">
                {t.status === 'completed' ? '✅' : t.status === 'in_progress' ? '🔄' : '⏳'}
              </span>
              <span className={t.status === 'completed' ? 'text-muted-foreground/50 line-through' : 'text-foreground/80'}>
                {t.status === 'in_progress' ? (t.activeForm || t.content) : t.content}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
