import { BrowserControls } from '@/components/browser/BrowserControls'
import { ChatPanel } from '@/components/chat/ChatPanel'
import { TrafficList } from '@/components/network/TrafficList'
import { useBrowserRect } from '@/hooks/use-browser-rect'
import { useCdpEvents } from '@/hooks/use-cdp-events'

function App() {
  useCdpEvents()
  const browserRef = useBrowserRect()

  return (
    <div style={{ display: 'flex', height: '100vh', color: '#e8e8e8', background: '#0e0e0e' }}>
      <section style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
        <BrowserControls />
        <div style={{ flex: 1, display: 'flex', minHeight: 0 }}>
          <div
            ref={browserRef}
            style={{
              flex: 1,
              background: '#000',
              borderRight: '1px solid #333',
              minWidth: 0
            }}
          />
          <div style={{ width: 360, flexShrink: 0 }}>
            <TrafficList />
          </div>
        </div>
      </section>
      <aside style={{ width: 460, flexShrink: 0 }}>
        <ChatPanel />
      </aside>
    </div>
  )
}

export default App
