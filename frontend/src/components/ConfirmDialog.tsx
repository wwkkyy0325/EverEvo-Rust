// Permission confirmation dialog — appears when the sandbox (SemiAuto mode)
// requires user approval for a dangerous command or external path access.

import { useStore } from '../store';

export default function ConfirmDialog() {
  const confirmRequest = useStore((s) => s.confirmRequest);
  const confirmCommand = useStore((s) => s.confirmCommand);

  if (!confirmRequest) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-background border border-warning/50 rounded-xl p-5 max-w-lg w-full mx-4 shadow-2xl">
        <div className="flex items-start gap-3 mb-3">
          <span className="text-2xl shrink-0">⚠️</span>
          <div>
            <h2 className="text-sm font-bold text-warning">确认操作</h2>
            <p className="text-xs text-muted-foreground mt-1">{confirmRequest.reason}</p>
          </div>
        </div>

        <div className="bg-secondary rounded p-3 mb-3">
          <code className="text-xs text-foreground break-all">{confirmRequest.command}</code>
        </div>

        <p className="text-xs text-muted-foreground mb-3">
          此命令被半自动模式拦截。请选择是否允许执行。
        </p>

        <div className="flex gap-2 justify-end">
          <button
            onClick={() => confirmCommand(false)}
            className="px-4 py-2 rounded text-sm bg-secondary hover:bg-secondary/80 transition-colors"
          >
            拒绝
          </button>
          <button
            onClick={() => confirmCommand(true)}
            className="px-4 py-2 rounded text-sm bg-warning hover:bg-warning/90 text-warning-foreground font-medium transition-colors"
          >
            允许执行
          </button>
        </div>
      </div>
    </div>
  );
}
