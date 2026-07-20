import { useStore } from '../store';

export default function AuditPanel() {
  const { auditRecords, auditTotal, showAudit, toggleAudit, loadAudit, activeSessionId } = useStore();

  if (!showAudit) return null;

  return (
    <div className="fixed inset-0 z-40 flex">
      {/* Backdrop */}
      <div className="flex-1 bg-black/40" onClick={toggleAudit} />
      {/* Panel */}
      <div className="w-[420px] h-full bg-gray-950 border-l border-gray-800 flex flex-col shadow-2xl">
        <header className="p-3 border-b border-gray-800 flex items-center justify-between shrink-0">
          <div>
            <h2 className="text-sm font-bold text-gray-200">审计日志</h2>
            <p className="text-xs text-gray-500">{auditTotal} 条记录</p>
          </div>
          <div className="flex gap-2">
            <button
              onClick={() => activeSessionId && loadAudit(activeSessionId)}
              className="text-xs text-blue-400 hover:text-blue-300 px-2 py-1 rounded bg-blue-900/30"
            >
              刷新
            </button>
            <button onClick={toggleAudit} className="text-gray-400 hover:text-white text-lg leading-none">×</button>
          </div>
        </header>

        <main className="flex-1 overflow-y-auto p-2 space-y-1.5">
          {auditRecords.length === 0 && (
            <p className="text-xs text-gray-600 text-center py-8">暂无记录。执行 shell 命令后自动生成。</p>
          )}
          {auditRecords.map((r, i) => (
            <div key={i} className={`p-2 rounded text-xs border ${
              r.exit_code === 0 ? 'border-gray-800 bg-gray-900/50' :
              r.exit_code === 126 ? 'border-red-900/50 bg-red-950/20' :
              'border-yellow-900/50 bg-yellow-950/20'
            }`}>
              <div className="flex items-center gap-2 mb-1">
                <span className={`font-mono font-bold ${r.exit_code === 0 ? 'text-green-400' : r.exit_code === 126 ? 'text-red-400' : 'text-yellow-400'}`}>
                  {r.exit_code === 0 ? '✓' : '✗'} {r.exit_code}
                </span>
                <span className="text-gray-500">{r.shell}</span>
                <span className="text-gray-600 ml-auto">{r.duration_ms}ms</span>
              </div>
              <code className="block text-gray-300 break-all bg-gray-950/50 p-1 rounded">{r.command}</code>
              <div className="flex gap-3 mt-1 text-gray-600">
                <span>{r.permission_level}</span>
                <span>{r.decision}</span>
                <span>out:{r.stdout_len}B</span>
                {r.stderr_len > 0 && <span className="text-red-500">err:{r.stderr_len}B</span>}
              </div>
            </div>
          ))}
        </main>
      </div>
    </div>
  );
}
