import { useState, useEffect, useRef } from 'react';
import BootstrapView from './components/BootstrapView';
import ChatView from './components/ChatView';
import SettingsView from './components/SettingsView';
import DevPanel from './components/DevPanel';
import SessionSidebar from './components/SessionSidebar';
import ConfirmDialog from './components/ConfirmDialog';
import AskUserDialog from './components/AskUserDialog';
import ErrorBoundary from './components/ErrorBoundary';
import AppStatusBar from './components/AppStatusBar';
import { Dialog } from './components/ui/dialog';

type View = 'bootstrap' | 'chat' | 'settings' | 'devpanel';
type SettingsTab = 'llm' | 'routing' | 'character';

function App() {
  const [view, setView] = useState<View>('bootstrap');
  const [settingsTab, setSettingsTab] = useState<SettingsTab>('llm');
  const [showLlmPrompt, setShowLlmPrompt] = useState(false);
  const llmPromptShown = useRef(false);

  // After bootstrap, check if LLM is configured — only once
  useEffect(() => {
    if (view === 'bootstrap' || llmPromptShown.current) return;
    let cancelled = false;

    async function check() {
      try {
        const r = await fetch('/api/config');
        const data = await r.json();
        if (cancelled) return;
        if (!data.has_llm) {
          llmPromptShown.current = true;
          setTimeout(() => { if (!cancelled) setShowLlmPrompt(true); }, 800);
        }
      } catch { /* ignore */ }
    }
    check();
    return () => { cancelled = true; };
  }, [view]);

  if (view === 'bootstrap') {
    return <BootstrapView onEnter={() => setView('chat')} />;
  }

  return (
    <div className="flex flex-col h-screen overflow-hidden bg-background text-foreground animate-fade-in">
      {/* Top: sidebar + main */}
      <div className="flex flex-1 min-h-0 overflow-hidden">
        <SessionSidebar view={view} settingsTab={settingsTab} onSettingsTabChange={setSettingsTab} />

        <div className="flex-1 flex flex-col min-w-0 overflow-hidden">
          <nav className="shrink-0 bg-background px-4 py-2 flex items-center justify-start gap-2">
            <button
              onClick={() => setView('chat')}
              className={`text-xs px-3 py-1 rounded transition-colors ${
                view === 'chat'
                  ? 'bg-primary text-primary-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
            >
              聊天
            </button>
            <button
              onClick={() => setView('settings')}
              className={`text-xs px-3 py-1 rounded transition-colors ${
                view === 'settings'
                  ? 'bg-primary text-primary-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
            >
              设置
            </button>
            <button
              onClick={() => setView('devpanel')}
              className={`text-xs px-3 py-1 rounded transition-colors ${
                view === 'devpanel'
                  ? 'bg-primary text-primary-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
            >
              🔧 诊断
            </button>
          </nav>

          {view === 'chat' && <ErrorBoundary><ChatView /></ErrorBoundary>}
          {view === 'settings' && <ErrorBoundary><SettingsView settingsTab={settingsTab} /></ErrorBoundary>}
          {view === 'devpanel' && <ErrorBoundary><DevPanel /></ErrorBoundary>}
        </div>
      </div>

      {/* Bottom status bar */}
      <AppStatusBar />

      {/* Dialogs */}
      <ConfirmDialog />
      <AskUserDialog />

      {/* LLM config prompt — shown after bootstrap if no LLM configured */}
      <Dialog
        open={showLlmPrompt}
        onClose={() => setShowLlmPrompt(false)}
        title="⚙️ 配置大模型"
        persistent
      >
        <div className="flex flex-col gap-4">
          <p className="text-xs text-muted-foreground leading-relaxed">
            检测到尚未配置大模型。EverEvo 需要至少一个 LLM 提供商才能进行对话和智能操作。
          </p>
          <p className="text-xs text-muted-foreground">
            支持 OpenAI 兼容接口（DeepSeek、OpenAI 等）和 Anthropic 原生接口。
          </p>
          <div className="flex gap-2 pt-1">
            <button
              onClick={() => { setShowLlmPrompt(false); setView('settings'); }}
              className="flex-1 py-2 rounded-lg text-xs font-medium bg-primary hover:bg-primary/90 text-primary-foreground transition-colors"
            >
              去配置
            </button>
            <button
              onClick={() => setShowLlmPrompt(false)}
              className="px-3 py-2 rounded-lg text-xs bg-secondary hover:bg-secondary/80 text-muted-foreground hover:text-foreground transition-colors"
            >
              稍后再说
            </button>
          </div>
        </div>
      </Dialog>
    </div>
  );
}

export default App;
