import { useChat } from '@ai-sdk/react'
import { useMemo, useState, type FormEvent } from 'react'

import { ACPChatTransport } from '@/ai/acp-transport'
import { ACP_AGENTS, type ACPAgentID } from '@/constants'

export function ChatPanel() {
  const [agentId, setAgentId] = useState<ACPAgentID>('claude-code')
  const [input, setInput] = useState('')

  const transport = useMemo(() => {
    const agentDef = ACP_AGENTS.find((a) => a.id === agentId)!
    return new ACPChatTransport({ agentDef })
  }, [agentId])

  const { messages, sendMessage, status } = useChat({ transport })

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    const text = input.trim()
    if (!text || status === 'streaming' || status === 'submitted') return
    setInput('')
    void sendMessage({ text })
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', borderLeft: '1px solid #333' }}>
      <header style={{ padding: '8px 12px', borderBottom: '1px solid #333', display: 'flex', gap: 8, alignItems: 'center' }}>
        <strong>Agent</strong>
        <select
          value={agentId}
          onChange={(e) => setAgentId(e.target.value as ACPAgentID)}
          disabled={status === 'streaming' || status === 'submitted'}
        >
          {ACP_AGENTS.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </select>
        <span style={{ marginLeft: 'auto', opacity: 0.6, fontSize: 12 }}>{status}</span>
      </header>

      <div style={{ flex: 1, overflowY: 'auto', padding: 12 }}>
        {messages.length === 0 && (
          <p style={{ opacity: 0.5 }}>
            안녕하세요. 브라우저를 띄우고 분석할 사이트를 알려주시거나, 이미 한 행동을 분석해달라고 요청하세요.
          </p>
        )}
        {messages.map((m) => (
          <div key={m.id} style={{ marginBottom: 12 }}>
            <div style={{ fontSize: 11, opacity: 0.6, marginBottom: 4 }}>{m.role}</div>
            {m.parts.map((part, i) => {
              if (part.type === 'text') {
                return (
                  <div key={i} style={{ whiteSpace: 'pre-wrap' }}>
                    {part.text}
                  </div>
                )
              }
              if (part.type === 'reasoning') {
                return (
                  <div key={i} style={{ whiteSpace: 'pre-wrap', opacity: 0.6, fontStyle: 'italic' }}>
                    {part.text}
                  </div>
                )
              }
              if (part.type.startsWith('tool-')) {
                return (
                  <pre key={i} style={{ background: '#1a1a1a', padding: 8, fontSize: 12, overflow: 'auto' }}>
                    {JSON.stringify(part, null, 2)}
                  </pre>
                )
              }
              return null
            })}
          </div>
        ))}
      </div>

      <form onSubmit={onSubmit} style={{ display: 'flex', gap: 8, padding: 12, borderTop: '1px solid #333' }}>
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="메시지 입력…"
          style={{ flex: 1, padding: '6px 10px' }}
          disabled={status === 'streaming' || status === 'submitted'}
        />
        <button type="submit" disabled={status === 'streaming' || status === 'submitted' || !input.trim()}>
          전송
        </button>
      </form>
    </div>
  )
}
