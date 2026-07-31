// Global bottom status bar — thin, quiet, informative.

import { useStore } from '../store';

export default function AppStatusBar() {
  const sandboxShell = useStore((s) => s.sandboxShell);
  const sandboxLevel = useStore((s) => s.sandboxLevel);
  const activeSessions = useStore((s) => s.activeSessions);
  const sessionCount = useStore((s) => s.sessions.length);
  const toolCount = useStore((s) => s.toolCount);
  const mcpServers = useStore((s) => s.mcpServers);
  const planMode = useStore((s) => s.planMode);
  const planTask = useStore((s) => s.planTask);
  const activeSessionId = useStore((s) => s.activeSessionId);
  const sessions = useStore((s) => s.sessions);
  const bindSessionWorkspace = useStore((s) => s.bindSessionWorkspace);

  // Per-session workspace: find active session and show its workspace_dir
  const activeSession = sessions.find((s) => s.id === activeSessionId);
  const sessionWorkspace = activeSession?.workspace_dir ?? null;

  const isWsl = sandboxShell.toLowerCase().includes('wsl');
  const isLinuxNative = sandboxShell === '/bin/sh' || sandboxShell === 'sh';
  const mcpConnected = mcpServers.filter((s) => s.status === 'connected').length;

  const handleSetWorkspace = () => {
    if (!activeSessionId) return;
    const current = sessionWorkspace || '';
    const path = prompt('Workspace directory (absolute path, empty to reset):', current);
    if (path !== null) {
      bindSessionWorkspace(activeSessionId, path.trim() || null);
    }
  };

  return (
    <div className="shrink-0 border-t border-border bg-statusbar px-4 py-1 flex items-center gap-4 text-[10px] text-statusbar-foreground">
      {/* Per-session workspace indicator */}
      <span
        className="cursor-pointer hover:text-foreground"
        onClick={handleSetWorkspace}
        title="Click to set workspace for current session"
      >
        {sessionWorkspace ? `📂 ${sessionWorkspace}` : '🏖️ sandbox'}
      </span>
      {/* Plan mode indicator */}
      {planMode && (
        <span className="text-warning font-medium" title={planTask ? `Planning: ${planTask}` : 'Plan mode active — read-only exploration'}>
          📋 Plan {planTask ? `— ${planTask.slice(0, 30)}${planTask.length > 30 ? '…' : ''}` : 'Mode'}
        </span>
      )}
      {/* Shell indicator */}
      <span className={isWsl || isLinuxNative ? 'text-success' : ''}>
        {isWsl ? '🐧 WSL' : isLinuxNative ? '🐧 Linux' : sandboxShell === '...' ? '⌛' : `💻 ${sandboxShell}`}
      </span>
      <span>🔒 {sandboxLevel}</span>
      <span>🔧 {toolCount} tools</span>
      {mcpConnected > 0 && (
        <span className="text-success">🔌 {mcpConnected} MCP</span>
      )}
      <span className="ml-auto">{sessionCount} sessions</span>
      <span>📦 {activeSessions} active</span>
    </div>
  );
}
