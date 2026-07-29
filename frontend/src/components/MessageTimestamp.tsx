/** Relative timestamp matching Claude Code's style. */
export default function MessageTimestamp({ createdAt }: { createdAt: string }) {
  if (!createdAt) return null;
  const diff = Date.now() - new Date(createdAt).getTime();
  const secs = Math.floor(diff / 1000);
  if (secs < 60) return <span className="text-[10px] text-muted-foreground/50">{secs}s</span>;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return <span className="text-[10px] text-muted-foreground/50">{mins}m</span>;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return <span className="text-[10px] text-muted-foreground/50">{hours}h</span>;
  const days = Math.floor(hours / 24);
  return <span className="text-[10px] text-muted-foreground/50">{days}d</span>;
}
