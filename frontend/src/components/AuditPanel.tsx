import { useStore } from '../store';

export default function AuditPanel() {
  const { auditRecords, auditTotal, showAudit, toggleAudit, loadAudit, activeSessionId } = useStore();

  if (!showAudit) return null;

  return (
    <div className="fixed inset-0 z-40 flex">
      {/* Backdrop */}
      <div className="flex-1 bg-black/40" onClick={toggleAudit} />
      {/* Panel */}
      <div className="w-[420px] h-full bg-sidebar border-l border-border flex flex-col shadow-2xl">
        <header className="p-3 border-b border-border flex items-center justify-between shrink-0">
          <div>
            <h2 className="text-sm font-bold text-foreground">审计日志</h2>
            <p className="text-xs text-muted-foreground">{auditTotal} 条记录</p>
          </div>
          <div className="flex gap-2">
            <button
              onClick={() => activeSessionId && loadAudit(activeSessionId)}
              className="text-xs text-primary hover:text-primary/80 px-2 py-1 rounded bg-primary/20"
            >
              刷新
            </button>
            <button onClick={toggleAudit} className="text-muted-foreground hover:text-foreground text-lg leading-none">×</button>
          </div>
        </header>

        <main className="flex-1 overflow-y-auto p-2 space-y-1.5">
          {auditRecords.length === 0 && (
            <p className="text-xs text-muted-foreground text-center py-8">暂无记录。执行 shell 命令后自动生成。</p>
          )}
          {auditRecords.map((r, i) => (
            <div key={i} className={`p-2 rounded text-xs border ${
              r.exit_code === 0 ? 'border-border bg-secondary/50' :
              r.exit_code === 126 ? 'border-destructive/30 bg-destructive/5' :
              'border-warning/30 bg-warning/5'
            }`}>
              <div className="flex items-center gap-2 mb-1">
                <span className={`font-mono font-bold ${r.exit_code === 0 ? 'text-success' : r.exit_code === 126 ? 'text-destructive' : 'text-warning'}`}>
                  {r.exit_code === 0 ? '✓' : '✗'} {r.exit_code}
                </span>
                <span className="text-muted-foreground">{r.shell}</span>
                <span className="text-muted-foreground/60 ml-auto">{r.duration_ms}ms</span>
              </div>
              <code className="block text-foreground break-all bg-background/50 p-1 rounded">{r.command}</code>
              <div className="flex gap-3 mt-1 text-muted-foreground">
                <span>{r.permission_level}</span>
                <span>{r.decision}</span>
                <span>out:{r.stdout_len}B</span>
                {r.stderr_len > 0 && <span className="text-destructive">err:{r.stderr_len}B</span>}
              </div>
            </div>
          ))}
        </main>
      </div>
    </div>
  );
}
