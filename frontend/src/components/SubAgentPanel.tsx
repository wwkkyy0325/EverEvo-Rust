import { useEffect, useRef } from 'react';
import { useStore } from '../store';

/**
 * Shows running/completed sub-agent tasks. Polls /api/agent/tasks
 * every 3 seconds as a fallback for missed SSE events.
 */
export default function SubAgentPanel() {
  const tasks = useStore((s) => s.subagentTasks || []);
  const streaming = useStore((s) => s.streaming);
  const activeSessionId = useStore((s) => s.activeSessionId);
  const interval = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    if (streaming && activeSessionId) {
      interval.current = setInterval(async () => {
        try {
          const r = await fetch(`/api/agent/tasks?session_id=${activeSessionId}`);
          const j = await r.json();
          if (j.data?.tasks) {
            useStore.setState({ subagentTasks: j.data.tasks.map((t: any) => ({
              id: t.id, description: t.description,
              status: t.status, result: t.result,
            }))});
          }
        } catch { /* ignore */ }
      }, 3000);
    }
    return () => { if (interval.current) clearInterval(interval.current); };
  }, [streaming, activeSessionId]);

  if (tasks.length === 0) return null;

  const running = tasks.filter((t) => t.status === 'running');
  const done = tasks.filter((t) => t.status !== 'running');

  return (
    <div className="px-4 py-1">
      <div className="rounded-lg border border-border/50 bg-muted/20 overflow-hidden">
        <div className="px-3 py-1.5 flex items-center gap-2 text-[10px] text-muted-foreground">
          <span className="font-medium">子任务</span>
          {running.length > 0 && (
            <span className="flex items-center gap-1">
              <span className="inline-block w-1.5 h-1.5 rounded-full bg-warning animate-pulse" />
              {running.length} 运行中
            </span>
          )}
          {done.length > 0 && <span>{done.length} 已完成</span>}
        </div>
        <div className="border-t border-border/30 px-3 py-1 space-y-0.5">
          {tasks.map((t) => (
            <div key={t.id} className="flex items-center gap-2 text-[10px]">
              <span className="shrink-0">
                {t.status === 'running' ? '🔄' : '✅'}
              </span>
              <span className={t.status === 'running' ? 'text-foreground/70' : 'text-muted-foreground/50'}>
                {t.description}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
