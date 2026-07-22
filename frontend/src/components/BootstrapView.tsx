// BootstrapView — splash screen: wait for backend assets, then enter.
// LLM config prompt is handled separately after app entry.

import { useState, useEffect, useRef, useCallback } from 'react';

interface Props { onEnter: () => void; }

const MIN_SPLASH_MS = 3000;
const TOTAL = 6;

export default function BootstrapView({ onEnter }: Props) {
  const [phase, setPhase] = useState<'connecting' | 'downloading' | 'waiting' | 'leaving'>('connecting');
  const [pct, setPct] = useState(0);
  const [done, setDone] = useState(0);
  const [total, setTotal] = useState(TOTAL);

  const leavingRef = useRef(false);
  const readyRef = useRef(false);
  const minTimeRef = useRef(false);
  const esRef = useRef<EventSource | null>(null);

  const tryLeave = useCallback(() => {
    if (leavingRef.current) return;
    if (readyRef.current && minTimeRef.current) {
      leavingRef.current = true;
      setPhase('leaving');
      setTimeout(onEnter, 600);
    }
  }, [onEnter]);

  // 3s minimum display
  useEffect(() => {
    const t = setTimeout(() => { minTimeRef.current = true; tryLeave(); }, MIN_SPLASH_MS);
    return () => clearTimeout(t);
  }, [tryLeave]);

  // Poll init status + SSE for downloads
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;

    async function tick() {
      if (cancelled) return;
      try {
        const r = await fetch('/api/init/status');
        const data = await r.json();
        if (cancelled) return;
        const bp: string = data.phase;

        if (bp === 'ready' || bp === 'checking') {
          readyRef.current = true;
          setDone(TOTAL); setTotal(TOTAL); setPct(100);
          setPhase('waiting');
          tryLeave();
          return;
        }
        if (bp === 'waiting_llm' || bp === 'ready') {
          // Backend might still be waiting; treat as ready for frontend
          readyRef.current = true;
          setPhase('waiting');
          tryLeave();
          return;
        }

        setPhase('downloading');
        // Connect SSE for download progress
        if (!esRef.current || esRef.current.readyState === EventSource.CLOSED) {
          const es = new EventSource('/api/bootstrap/download');
          esRef.current = es;
          es.addEventListener('progress', (e: MessageEvent) => {
            const { percentage } = JSON.parse(e.data);
            setPct((prev) => Math.max(prev, Math.round(percentage)));
          });
          es.addEventListener('done', () => { es.close(); readyRef.current = true; setPct(100); tryLeave(); });
          es.addEventListener('error', () => {});
        }
        // Check asset counts
        try {
          const ar = await fetch('/api/bootstrap/status');
          const ad = await ar.json();
          if (ad.all_ready) {
            readyRef.current = true;
            setDone(TOTAL); setTotal(TOTAL);
            tryLeave();
          } else {
            setDone(ad.ready_count ?? 0);
            setTotal((ad.ready_count ?? 0) + (ad.missing_count ?? 0) + (ad.corrupt_count ?? 0));
          }
        } catch {}
      } catch {
        // backend not ready yet
      }
      if (!cancelled) timer = setTimeout(tick, 1500);
    }
    tick();
    return () => { cancelled = true; clearTimeout(timer); esRef.current?.close(); };
  }, [tryLeave]);

  const label: Record<string, string> = {
    connecting: '连接服务', downloading: '下载组件', waiting: '即将进入', leaving: '',
  };

  const leaving = phase === 'leaving';
  const showProgress = phase === 'downloading' || phase === 'waiting';

  return (
    <div className={`flex flex-col items-center justify-center h-screen gap-6 bg-background px-6 select-none ${leaving ? 'animate-fade-out' : ''}`}>
      <div className="text-4xl">🦾</div>
      <div className="flex items-center gap-2">
        <span className={`inline-block w-2 h-2 ${phase === 'connecting' ? 'animate-pulse bg-warning' : phase === 'waiting' ? 'bg-success' : 'bg-success'}`} />
        <span className="text-xs text-muted-foreground font-mono tracking-wider uppercase">{label[phase]}</span>
      </div>
      {showProgress && (
        <div className="flex flex-col items-center gap-2 w-full max-w-xs">
          <div className="w-full bg-secondary rounded-none h-2.5 overflow-hidden border border-border">
            <div className="bg-primary h-full transition-all duration-700 ease-out" style={{ width: `${Math.max(pct, 2)}%` }} />
          </div>
          <p className="text-xl font-bold text-foreground font-mono">{phase === 'waiting' ? 'OK' : `${pct}%`}</p>
          <p className="text-xs text-muted-foreground">{done}/{total} 项</p>
        </div>
      )}
      {phase === 'connecting' && <p className="text-[11px] text-muted-foreground/50">等待后端服务...</p>}
    </div>
  );
}
