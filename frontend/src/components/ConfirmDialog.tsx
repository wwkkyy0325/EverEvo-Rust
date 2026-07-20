// Permission confirmation dialog — appears when the sandbox (SemiAuto mode)
// requires user approval for a dangerous command or external path access.
//
// The sandbox tool blocks internally on a oneshot channel. This dialog is
// shown by the SSE `confirmation_required` event, and the user's Allow/Deny
// response is sent via POST /api/sandbox/sessions/{id}/confirm.
//
// Pattern: Claude Code permission dialog — Allow / Deny buttons,
// transparent to the LLM (the LLM never sees the confirmation).

import { useStore } from '../store';

export default function ConfirmDialog() {
  const confirmRequest = useStore((s) => s.confirmRequest);
  const confirmCommand = useStore((s) => s.confirmCommand);

  if (!confirmRequest) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-gray-900 border border-orange-700 rounded-xl p-5 max-w-lg w-full mx-4 shadow-2xl">
        <div className="flex items-start gap-3 mb-3">
          <span className="text-2xl shrink-0">⚠️</span>
          <div>
            <h2 className="text-sm font-bold text-orange-300">确认操作</h2>
            <p className="text-xs text-gray-400 mt-1">{confirmRequest.reason}</p>
          </div>
        </div>

        <div className="bg-gray-950 rounded p-3 mb-3">
          <code className="text-xs text-gray-300 break-all">{confirmRequest.command}</code>
        </div>

        <p className="text-xs text-gray-500 mb-3">
          此命令被半自动模式拦截。请选择是否允许执行。
        </p>

        <div className="flex gap-2 justify-end">
          <button
            onClick={() => confirmCommand(false)}
            className="px-4 py-2 rounded text-sm bg-gray-700 hover:bg-gray-600 transition-colors"
          >
            拒绝
          </button>
          <button
            onClick={() => confirmCommand(true)}
            className="px-4 py-2 rounded text-sm bg-orange-600 hover:bg-orange-500 font-medium transition-colors"
          >
            允许执行
          </button>
        </div>
      </div>
    </div>
  );
}
