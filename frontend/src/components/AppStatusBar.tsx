// Global bottom status bar — full width, thin, shows sandbox + session info.

import { useStore } from '../store';

export default function AppStatusBar() {
  const sandboxShell = useStore((s) => s.sandboxShell);
  const sandboxLevel = useStore((s) => s.sandboxLevel);
  const activeSessions = useStore((s) => s.activeSessions);
  const sessionCount = useStore((s) => s.sessions.length);

  return (
    <div className="shrink-0 border-t border-border bg-statusbar px-4 py-1 flex items-center gap-4 text-[10px] text-statusbar-foreground">
      <span>🖥 {sandboxShell === 'none' ? '...' : sandboxShell}</span>
      <span>🔒 {sandboxLevel}</span>
      <span className="ml-auto">{sessionCount} sessions</span>
      <span>📦 {activeSessions} active</span>
    </div>
  );
}
