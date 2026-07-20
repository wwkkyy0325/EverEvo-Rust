import { useState, useEffect, useRef, useCallback } from 'react';

// ── Types ──────────────────────────────────────────────────────────

interface AssetItem {
  key: string;
  name: string;
  version: string;
  category: 'runtime' | 'model';
  status: 'ready' | 'missing' | 'corrupt' | 'downloading' | 'extracting' | 'done';
  size_mb: number;
  description: string;
  progress?: number;       // 0–100
  speed_mb?: number;
  downloaded_mb?: number;
}

interface BootstrapStatus {
  all_ready: boolean;
  ready_count: number;
  missing_count: number;
  corrupt_count: number;
  total_download_mb: number;
  assets: AssetItem[];
}

// ── Props ──────────────────────────────────────────────────────────

interface Props {
  onEnter: () => void;
}

// ── Component ──────────────────────────────────────────────────────

export default function BootstrapView({ onEnter }: Props) {
  const [status, setStatus] = useState<BootstrapStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(true);
  const [downloading, setDownloading] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState<Record<string, number>>({});
  const [_downloadSpeeds, setDownloadSpeeds] = useState<Record<string, number>>({});
  const [completedCount, setCompletedCount] = useState(0);
  const [totalCount, setTotalCount] = useState(0);
  const eventSource = useRef<EventSource | null>(null);

  // ── Initial check ────────────────────────────────────────────
  useEffect(() => {
    let cancelled = false;
    let attempt = 0;
    const maxAttempts = 30; // up to ~60s (30 × 2s)

    async function pollStatus() {
      while (attempt < maxAttempts && !cancelled) {
        attempt++;
        try {
          const r = await fetch('/api/bootstrap/status');
          const data: BootstrapStatus = await r.json();
          if (cancelled) return;
          setStatus(data);
          setChecking(false);
          if (data.all_ready) {
            setTimeout(onEnter, 1500);
          }
          return;
        } catch {
          // Backend not ready yet — wait and retry
          await new Promise((r) => setTimeout(r, 2000));
        }
      }
      if (!cancelled) {
        setError('无法连接后端，请确认服务已启动');
        setChecking(false);
      }
    }

    pollStatus();
    return () => { cancelled = true; };
  }, []);

  // ── Cleanup SSE on unmount ───────────────────────────────────
  useEffect(() => {
    return () => {
      eventSource.current?.close();
    };
  }, []);

  // ── Start Download ───────────────────────────────────────────
  const startDownload = useCallback(() => {
    setDownloading(true);
    const es = new EventSource('/api/bootstrap/download');
    eventSource.current = es;

    // Update status to "downloading" for missing assets
    setStatus((prev) => {
      if (!prev) return prev;
      return {
        ...prev,
        assets: prev.assets.map((a) =>
          a.status === 'missing' || a.status === 'corrupt'
            ? { ...a, status: 'downloading', progress: 0 }
            : a
        ),
      };
    });

    es.addEventListener('start', (e) => {
      const data = JSON.parse(e.data);
      setTotalCount(data.total);
    });

    es.addEventListener('queued', (e) => {
      const { key } = JSON.parse(e.data);
      setStatus((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          assets: prev.assets.map((a) =>
            a.key === key ? { ...a, status: 'downloading', progress: 0 } : a
          ),
        };
      });
    });

    es.addEventListener('progress', (e) => {
      const { key, percentage, speed_mb } = JSON.parse(e.data);
      setDownloadProgress((prev) => ({ ...prev, [key]: percentage }));
      setDownloadSpeeds((prev) => ({ ...prev, [key]: speed_mb }));
    });

    es.addEventListener('extracting', (e) => {
      const { key } = JSON.parse(e.data);
      setStatus((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          assets: prev.assets.map((a) =>
            a.key === key ? { ...a, status: 'extracting' as const } : a
          ),
        };
      });
    });

    es.addEventListener('asset_done', (e) => {
      const { key, completed } = JSON.parse(e.data);
      setCompletedCount(completed);
      setStatus((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          assets: prev.assets.map((a) =>
            a.key === key ? { ...a, status: 'done' as const, progress: 100 } : a
          ),
        };
      });
    });

    es.addEventListener('asset_failed', (e) => {
      const { key, error } = JSON.parse(e.data);
      setStatus((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          assets: prev.assets.map((a) =>
            a.key === key ? { ...a, status: 'corrupt' as const } : a
          ),
        };
      });
      console.error(`Asset ${key} failed: ${error}`);
    });

    es.addEventListener('done', () => {
      es.close();
      setDownloading(false);
      // Refresh status
      fetch('/api/bootstrap/status')
        .then((r) => r.json())
        .then((data: BootstrapStatus) => {
          setStatus(data);
          if (data.all_ready) {
            setTimeout(onEnter, 2000);
          }
        });
    });

    es.addEventListener('error', (e: Event) => {
      try {
        const msg = e as MessageEvent;
        const data = JSON.parse(msg.data);
        setError(`下载失败: ${data.error || data.key}`);
      } catch {
        // Connection error — SSE will auto-reconnect
      }
    });

    es.onerror = () => {
      // SSE connection lost — it will auto-reconnect
    };
  }, [onEnter]);

  // ── Checking state ───────────────────────────────────────────
  if (checking) {
    return (
      <div className="flex flex-col items-center justify-center h-screen gap-6">
        <div className="animate-spin w-10 h-10 border-4 border-blue-500 border-t-transparent rounded-full" />
        <p className="text-gray-400 text-lg">正在检查运行时环境...</p>
      </div>
    );
  }

  // ── Error ────────────────────────────────────────────────────
  if (error) {
    return (
      <div className="flex flex-col items-center justify-center h-screen gap-6">
        <div className="text-red-400 text-6xl">⚠</div>
        <p className="text-red-400 text-lg">{error}</p>
        <button onClick={onEnter} className="bg-gray-700 hover:bg-gray-600 px-6 py-2 rounded-lg text-sm">
          跳过，直接进入
        </button>
      </div>
    );
  }

  if (!status) return null;

  // ── Main View ────────────────────────────────────────────────
  const overallPct =
    totalCount > 0 ? Math.round((completedCount / totalCount) * 100) : 0;

  return (
    <div className="flex flex-col h-screen max-w-2xl mx-auto p-6 gap-6">
      {/* Header */}
      <header className="text-center pt-8">
        <h1 className="text-2xl font-bold mb-2">EverEvo 首次启动</h1>
        {downloading ? (
          <div className="mt-3">
            <p className="text-gray-400 text-sm">
              正在下载 {completedCount}/{totalCount}（{overallPct}%）
            </p>
            <div className="w-full bg-gray-700 rounded-full h-2 mt-2 mx-auto max-w-xs">
              <div
                className="bg-blue-500 h-2 rounded-full transition-all duration-300"
                style={{ width: `${overallPct}%` }}
              />
            </div>
          </div>
        ) : (
          <p className="text-gray-400 text-sm">
            {status.missing_count + status.corrupt_count > 0
              ? `检测到 ${status.missing_count + status.corrupt_count} 个组件需要下载（约 ${status.total_download_mb} MB）`
              : '所有组件已就绪'}
          </p>
        )}
      </header>

      {/* Assets */}
      <main className="flex-1 overflow-y-auto">
        <div className="space-y-3">
          {['runtime', 'model'].map((category) => {
            const items = status.assets.filter((a) => a.category === category);
            if (items.length === 0) return null;
            return (
              <div key={category}>
                <h2 className="text-xs font-semibold text-gray-500 uppercase tracking-wider mt-4 mb-2">
                  {category === 'runtime' ? '运行时' : 'AI 模型'}
                </h2>
                {items.map((asset) => (
                  <AssetCard
                    key={asset.key}
                    asset={asset}
                    progress={downloadProgress[asset.key]}
                  />
                ))}
              </div>
            );
          })}
        </div>
      </main>

      {/* Actions */}
      <footer className="flex gap-3 pb-4">
        {!status.all_ready && !downloading && (
          <button
            onClick={startDownload}
            className="flex-1 bg-blue-600 hover:bg-blue-500 px-6 py-3 rounded-lg text-sm font-medium transition-colors"
          >
            下载全部
          </button>
        )}
        {downloading && (
          <div className="flex-1 bg-blue-600/50 text-center px-6 py-3 rounded-lg text-sm text-blue-200">
            下载中，请稍候...
          </div>
        )}
        <button
          onClick={onEnter}
          className={`${status.all_ready ? 'flex-1' : 'w-32'} bg-gray-700 hover:bg-gray-600 px-6 py-3 rounded-lg text-sm font-medium transition-colors`}
        >
          {status.all_ready ? '进入 EverEvo' : '跳过，先进入'}
        </button>
      </footer>
    </div>
  );
}

// ── Asset Card ────────────────────────────────────────────────────

function AssetCard({ asset, progress }: { asset: AssetItem; progress?: number }) {
  const icon =
    asset.status === 'ready' || asset.status === 'done'
      ? '✅'
      : asset.status === 'corrupt'
      ? '⚠️'
      : asset.status === 'extracting'
      ? '📦'
      : asset.status === 'downloading'
      ? '⏳'
      : '❌';

  const bg =
    asset.status === 'ready' || asset.status === 'done'
      ? 'bg-green-900/20 border-green-800'
      : asset.status === 'corrupt'
      ? 'bg-yellow-900/20 border-yellow-800'
      : asset.status === 'extracting'
      ? 'bg-purple-900/20 border-purple-700'
      : asset.status === 'downloading'
      ? 'bg-blue-900/20 border-blue-700'
      : 'bg-gray-800/50 border-gray-700';

  return (
    <div className={`p-3 rounded-lg border ${bg}`}>
      <div className="flex items-center gap-3">
        <span className="text-lg">{icon}</span>
        <div className="flex-1 min-w-0">
          <p className="text-sm font-medium truncate">{asset.name}</p>
          <p className="text-xs text-gray-500">
            v{asset.version}
            {asset.size_mb > 0 && ` · ${asset.size_mb} MB`}
          </p>
        </div>
        <StatusBadge status={asset.status} />
      </div>

      {/* Progress bar — show during download and extraction */}
      {(asset.status === 'downloading' || asset.status === 'extracting') && progress !== undefined && (
        <div className="mt-2">
          <div className="w-full bg-gray-700 rounded-full h-1.5">
            <div
              className="bg-blue-500 h-1.5 rounded-full transition-all duration-500"
              style={{ width: `${Math.min(progress, 100)}%` }}
            />
          </div>
          <p className="text-xs text-gray-500 mt-1 text-right">{progress.toFixed(1)}%</p>
        </div>
      )}
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const map: Record<string, { label: string; cls: string }> = {
    ready: { label: '就绪', cls: 'text-green-400 bg-green-900/30' },
    done: { label: '完成', cls: 'text-green-400 bg-green-900/30' },
    missing: { label: '待下载', cls: 'text-gray-400 bg-gray-700/50' },
    corrupt: { label: '需修复', cls: 'text-yellow-400 bg-yellow-900/30' },
    extracting: { label: '解压中', cls: 'text-purple-400 bg-purple-900/30' },
    downloading: { label: '下载中', cls: 'text-blue-400 bg-blue-900/30' },
  };
  const info = map[status] ?? { label: status, cls: 'text-gray-400 bg-gray-700/50' };

  return (
    <span className={`text-xs px-2 py-0.5 rounded ${info.cls}`}>
      {info.label}
    </span>
  );
}
