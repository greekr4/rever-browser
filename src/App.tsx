import { ChatPanel } from '@/components/chat/ChatPanel'

function App() {
  return (
    <div style={{ display: 'flex', height: '100vh', color: '#e8e8e8', background: '#0e0e0e' }}>
      <section style={{ flex: 1, padding: 16, display: 'flex', flexDirection: 'column' }}>
        <h1 style={{ fontSize: 18, margin: 0 }}>rever-browser</h1>
        <p style={{ opacity: 0.6, fontSize: 13 }}>
          M0.2: ACP 채팅만 검증. 좌측 패널(브라우저/트래픽)은 M0.3+ 에서 추가.
        </p>
      </section>
      <aside style={{ width: 480 }}>
        <ChatPanel />
      </aside>
    </div>
  )
}

export default App
