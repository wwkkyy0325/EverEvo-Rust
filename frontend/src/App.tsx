import { useState } from 'react';
import BootstrapView from './components/BootstrapView';
import ChatView from './components/ChatView';
import SettingsView from './components/SettingsView';
import SessionSidebar from './components/SessionSidebar';
import AuditPanel from './components/AuditPanel';
import ConfirmDialog from './components/ConfirmDialog';
import MemoryPanel from './components/MemoryPanel';
import DomainPanel from './components/DomainPanel';

type View = 'bootstrap' | 'chat' | 'settings';

function App() {
  const [view, setView] = useState<View>('bootstrap');

  if (view === 'bootstrap') {
    return <BootstrapView onEnter={() => setView('chat')} />;
  }

  return (
    <div className="flex h-screen bg-gray-950 text-white">
      {/* Session sidebar — always visible */}
      <SessionSidebar />

      {/* Main area */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Top nav bar */}
        <nav className="shrink-0 bg-gray-950/80 backdrop-blur border-b border-gray-800 px-4 py-2 flex items-center justify-end gap-2">
          <button
            onClick={() => setView('chat')}
            className={`text-xs px-3 py-1 rounded ${view === 'chat' ? 'bg-blue-800 text-blue-200' : 'text-gray-400 hover:text-white'}`}
          >
            聊天
          </button>
          <button
            onClick={() => setView('settings')}
            className={`text-xs px-3 py-1 rounded ${view === 'settings' ? 'bg-blue-800 text-blue-200' : 'text-gray-400 hover:text-white'}`}
          >
            设置
          </button>
        </nav>

        {/* Content */}
        {view === 'chat' && <ChatView />}
        {view === 'settings' && <SettingsView onBack={() => setView('chat')} />}
      </div>
      <AuditPanel />
      {/* Permission confirmation dialog — shown when sandbox SemiAuto requires user approval */}
      <ConfirmDialog />
      {/* Memory panel — browse/search persistent facts */}
      <MemoryPanel />
      {/* Domain panel — manage knowledge domains */}
      <DomainPanel />
    </div>
  );
}

export default App;
