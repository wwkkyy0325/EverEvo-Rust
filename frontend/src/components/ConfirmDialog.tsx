// Permission confirmation dialog — appears when the sandbox (SemiAuto mode)
// requires user approval for a dangerous command or external path access.
// Sticky buttons ensure confirmation is always reachable regardless of command length.

import { useStore } from '../store';

export default function ConfirmDialog() {
  const confirmQueue = useStore((s) => s.confirmQueue);
  const confirmCommand = useStore((s) => s.confirmCommand);

  // Read the front of the queue — multiple confirmations stack.
  const confirmRequest = confirmQueue[0] ?? null;
  if (!confirmRequest) return null;

  const queueLen = confirmQueue.length;

  const maxCmdLen = 600;
  const cmdDisplay = confirmRequest.command.length > maxCmdLen
    ? confirmRequest.command.slice(0, maxCmdLen) + '…'
    : confirmRequest.command;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
      <div className="bg-background border border-warning/50 rounded-xl shadow-2xl max-w-lg w-full max-h-[85vh] flex flex-col">
        {/* Header — fixed at top */}
        <div className="flex items-start gap-3 px-5 pt-5 pb-2 shrink-0">
          <span className="text-2xl shrink-0">⚠️</span>
          <div className="min-w-0 min-h-0">
            <span className="text-sm font-bold text-warning">确认操作{queueLen > 1 ? ` (${queueLen} 个待处理)` : ''}</span>
            <p className="text-xs text-muted-foreground mt-1 max-h-16 overflow-y-auto leading-relaxed">{confirmRequest.reason}</p>
          </div>
        </div>

        {/* Command — scrollable, can shrink to make room for buttons */}
        <div className="bg-secondary rounded mx-5 p-3 mb-3 max-h-40 min-h-0 overflow-y-auto flex-1">
          <code className="text-xs text-foreground break-all whitespace-pre-wrap">{cmdDisplay}</code>
        </div>

        {/* Hint — fixed */}
        <p className="text-xs text-muted-foreground px-5 mb-3 shrink-0">
          此命令被半自动模式拦截。请选择是否允许执行。
        </p>

        {/* Buttons — always visible at bottom */}
        <div className="flex gap-2 justify-end px-5 pb-5 pt-2 border-t border-border shrink-0">
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
