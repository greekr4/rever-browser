import { useChat } from '@ai-sdk/react'
import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { ACPChatTransport } from '@/ai/acp-transport'
import { useAcpAutoApprove, setAcpAutoApprove } from '@/ai/acp-permission'
import { ACP_AGENTS, type ACPAgentID } from '@/constants'
import { AgentPicker } from './AgentPicker'
import { formatOutput } from '@/lib/format-json'
import { useChatDraft } from '@/stores/chat-draft'

const SCROLL_BOTTOM_THRESHOLD = 32

function Thinking() {
  return (
    <div
      style={{
        display: 'flex',
        gap: 6,
        alignItems: 'center',
        padding: '6px 10px',
        marginBottom: 12,
        opacity: 0.7,
        fontSize: 12
      }}
    >
      <span className="thinking-dot" />
      <span className="thinking-dot" style={{ animationDelay: '0.15s' }} />
      <span className="thinking-dot" style={{ animationDelay: '0.3s' }} />
      <span style={{ marginLeft: 4 }}>Thinking…</span>
    </div>
  )
}

function Spinner() {
  return <span className="spinner" aria-hidden />
}

function CodeBlock({ children }: { children?: React.ReactNode }) {
  const [copied, setCopied] = useState(false)
  const ref = useRef<HTMLPreElement>(null)
  const onCopy = async () => {
    const text = ref.current?.innerText ?? ''
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      setTimeout(() => setCopied(false), 1200)
    } catch {}
  }
  return (
    <div style={{ position: 'relative' }}>
      <button
        type="button"
        onClick={onCopy}
        style={{
          position: 'absolute',
          top: 6,
          right: 6,
          padding: '2px 8px',
          fontSize: 11,
          background: '#1d1d1d',
          border: '1px solid #2e2e2e',
          color: '#bbb',
          borderRadius: 4,
          cursor: 'pointer',
          opacity: 0.85
        }}
      >
        {copied ? 'copied' : 'copy'}
      </button>
      <pre ref={ref}>{children}</pre>
    </div>
  )
}

const MD_COMPONENTS = {
  pre: ({ children }: { children?: React.ReactNode }) => <CodeBlock>{children}</CodeBlock>
}

function Markdown({ text, dim }: { text: string; dim?: boolean }) {
  return (
    <div className={`md${dim ? ' md--dim' : ''}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
        {text}
      </ReactMarkdown>
    </div>
  )
}

interface ToolPart {
  type: string
  toolName?: string
  toolCallId?: string
  state?: string
  input?: unknown
  output?: unknown
  errorText?: string
  title?: string
}

function ToolBlock({ part }: { part: ToolPart }) {
  const [open, setOpen] = useState(false)
  const name = part.title || part.toolName || part.type.replace(/^tool-/, '')
  const state = part.state ?? (part.output != null ? 'done' : part.errorText ? 'error' : '…')
  const stateColor =
    state === 'error' ? '#ff7676' : state === 'done' || state === 'output-available' ? '#7fd47f' : '#bbb'
  return (
    <div
      style={{
        background: '#161616',
        border: '1px solid #2a2a2a',
        borderRadius: 6,
        marginBottom: 6,
        fontSize: 12
      }}
    >
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        style={{
          width: '100%',
          display: 'flex',
          gap: 8,
          alignItems: 'center',
          padding: '6px 10px',
          background: 'transparent',
          border: 'none',
          color: '#ddd',
          cursor: 'pointer',
          textAlign: 'left',
          fontFamily: 'inherit',
          fontSize: 12
        }}
      >
        <span style={{ opacity: 0.6, width: 10 }}>{open ? '▾' : '▸'}</span>
        <span style={{ opacity: 0.6 }}>🔧</span>
        <strong style={{ fontWeight: 500 }}>{name}</strong>
        <span style={{ marginLeft: 'auto', color: stateColor, fontSize: 11 }}>{state}</span>
      </button>
      {open && (
        <div style={{ padding: '0 10px 8px 28px' }}>
          {part.input != null && (
            <details open style={{ marginTop: 4 }}>
              <summary style={{ cursor: 'pointer', opacity: 0.7 }}>input</summary>
              <pre style={preStyle}>{JSON.stringify(part.input, null, 2)}</pre>
            </details>
          )}
          {part.output != null && (
            <details open style={{ marginTop: 4 }}>
              <summary style={{ cursor: 'pointer', opacity: 0.7 }}>output</summary>
              <pre style={preStyle}>{formatOutput(part.output)}</pre>
            </details>
          )}
          {part.errorText && (
            <pre style={{ ...preStyle, color: '#ff7676' }}>{part.errorText}</pre>
          )}
        </div>
      )}
    </div>
  )
}

interface AnyPart {
  type: string
  text?: string
}

type Group =
  | { kind: 'text'; part: AnyPart; key: number }
  | { kind: 'work'; parts: AnyPart[]; key: number }

function groupParts(parts: AnyPart[]): Group[] {
  const groups: Group[] = []
  let buf: AnyPart[] = []
  parts.forEach((p, i) => {
    if (p.type === 'text') {
      if (buf.length) {
        groups.push({ kind: 'work', parts: buf, key: i - buf.length })
        buf = []
      }
      groups.push({ kind: 'text', part: p, key: i })
    } else {
      buf.push(p)
    }
  })
  if (buf.length) {
    groups.push({ kind: 'work', parts: buf, key: parts.length - buf.length })
  }
  return groups
}

function WorkGroup({ parts }: { parts: AnyPart[] }) {
  const [open, setOpen] = useState(false)
  const toolCount = parts.filter((p) => p.type.startsWith('tool-')).length
  const reasoningCount = parts.filter((p) => p.type === 'reasoning').length
  const summary = [
    toolCount > 0 ? `tools ${toolCount}` : null,
    reasoningCount > 0 ? `thoughts ${reasoningCount}` : null
  ]
    .filter(Boolean)
    .join(' · ')

  return (
    <div style={{ margin: '4px 0' }}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        style={{
          display: 'inline-flex',
          gap: 6,
          alignItems: 'center',
          padding: '2px 10px',
          fontSize: 11,
          background: 'transparent',
          border: '1px solid #2a2a2a',
          borderRadius: 12,
          color: '#aaa',
          cursor: 'pointer'
        }}
      >
        <span>{open ? '▾' : '▸'}</span>
        <span>{summary || 'work'}</span>
        <span style={{ opacity: 0.5 }}>· {open ? 'hide' : 'show more'}</span>
      </button>
      {open && (
        <div style={{ marginTop: 6, paddingLeft: 8, borderLeft: '2px solid #222' }}>
          {parts.map((p, i) => {
            if (p.type === 'reasoning') return <Markdown key={i} text={p.text ?? ''} dim />
            if (p.type.startsWith('tool-')) return <ToolBlock key={i} part={p as ToolPart} />
            return null
          })}
        </div>
      )}
    </div>
  )
}

const preStyle: React.CSSProperties = {
  background: '#0c0c0c',
  padding: 8,
  fontSize: 11,
  margin: '4px 0 0',
  overflowX: 'auto',
  maxHeight: 240,
  overflowY: 'auto',
  borderRadius: 4,
  whiteSpace: 'pre-wrap',
  wordBreak: 'break-word'
}

interface ModelEntry {
  modelId: string
  name: string
}

export function ChatPanel() {
  const [agentId, setAgentId] = useState<ACPAgentID>('claude-code')
  // Absolute path returned by detectAgents(). When set, we pass it to the
  // transport instead of the bare bin name so spawn doesn't depend on the
  // Electron child process's PATH (notably broken on Windows for .cmd shims).
  const [agentBinPath, setAgentBinPath] = useState<string | null>(null)
  const [input, setInput] = useState('')
  const [autoScroll, setAutoScroll] = useState(true)
  const [models, setModels] = useState<ModelEntry[]>([])
  const [currentModel, setCurrentModel] = useState<string | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)

  const transport = useMemo(() => {
    const base = ACP_AGENTS.find((a) => a.id === agentId)!
    const agentDef = agentBinPath ? { ...base, command: agentBinPath } : base
    return new ACPChatTransport({ agentDef })
  }, [agentId, agentBinPath])

  // @ai-sdk/react's useChat only recreates its internal Chat when `id`
  // changes — a fresh `transport` prop alone is ignored. Without this key
  // change, switching agents keeps sending prompts to the old (still-alive)
  // session, which is why a Gemini-selected chat was answering as Claude.
  const chatKey = `${agentId}::${agentBinPath ?? ''}`

  // Tear down the previous ACP session when the agent (and thus transport)
  // changes, so we don't leak the old child process.
  useEffect(() => {
    return () => {
      void transport.reset()
    }
  }, [transport])

  const { messages, sendMessage, status, stop, setMessages } = useChat({
    id: chatKey,
    transport
  })
  const busy = status === 'streaming' || status === 'submitted'

  // Fetch model list whenever the session reaches a quiet state (= just spawned
  // or just finished a turn). The agent only exposes models AFTER newSession.
  useEffect(() => {
    if (status !== 'ready') return
    const sid = transport.getSessionId()
    if (!sid) {
      setModels([])
      setCurrentModel(null)
      return
    }
    let cancelled = false
    void window.rev.acp.modelState(sid).then((state) => {
      if (cancelled) return
      console.log('[chat] modelState ←', state)
      if (!state) {
        setModels([])
        setCurrentModel(null)
        return
      }
      setModels(state.availableModels.map((m) => ({ modelId: m.modelId, name: m.name })))
      setCurrentModel(state.currentModelId)
    })
    return () => {
      cancelled = true
    }
  }, [status, transport])

  const onChangeModel = async (modelId: string) => {
    const sid = transport.getSessionId()
    if (!sid) return
    setCurrentModel(modelId)
    try {
      await window.rev.acp.setModel(sid, modelId)
    } catch (e) {
      console.error('[acp] setModel failed', e)
    }
  }
  const waiting = status === 'submitted'
  const autoApprove = useAcpAutoApprove()
  const draftPending = useChatDraft((s) => s.pending)
  const consumeDraft = useChatDraft((s) => s.consume)

  const onReset = async () => {
    if (busy) {
      try {
        stop()
      } catch {}
    }
    await transport.reset()
    setMessages([])
    setInput('')
    setAutoScroll(true)
  }

  useEffect(() => {
    if (draftPending == null) return
    const text = consumeDraft()
    if (text == null) return
    setInput((prev) => (prev ? `${prev}\n${text}` : text))
  }, [draftPending, consumeDraft])

  const isAtBottom = () => {
    const el = scrollRef.current
    if (!el) return true
    return el.scrollHeight - el.scrollTop - el.clientHeight <= SCROLL_BOTTOM_THRESHOLD
  }

  const scrollToBottom = (behavior: ScrollBehavior = 'auto') => {
    const el = scrollRef.current
    if (!el) return
    el.scrollTo({ top: el.scrollHeight, behavior })
  }

  // Auto-scroll on new content while user hasn't scrolled away
  useEffect(() => {
    if (autoScroll) scrollToBottom()
  }, [messages, autoScroll, busy])

  const onScroll = () => {
    setAutoScroll(isAtBottom())
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    const text = input.trim()
    if (!text || busy) return
    setInput('')
    setAutoScroll(true)
    void sendMessage({ text })
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', borderLeft: '1px solid #333' }}>
      <header style={{ padding: '8px 12px', borderBottom: '1px solid #333', display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
        <AgentPicker
          agentId={agentId}
          disabled={busy}
          onChange={(id, resolvedPath) => {
            setAgentId(id)
            setAgentBinPath(resolvedPath)
          }}
        />
        <select
          value={currentModel ?? ''}
          onChange={(e) => void onChangeModel(e.target.value)}
          disabled={busy || models.length === 0}
          title={models.length === 0 ? 'Send a message to load models' : 'Switch model for this session'}
          style={{ maxWidth: 160 }}
        >
          {models.length === 0 ? (
            <option value="">no models yet</option>
          ) : (
            models.map((m) => (
              <option key={m.modelId} value={m.modelId}>
                {m.name}
              </option>
            ))
          )}
        </select>
        <span style={{ marginLeft: 'auto', opacity: 0.6, fontSize: 12 }}>{status}</span>
        <button
          type="button"
          onClick={() => setAcpAutoApprove(!autoApprove)}
          title={
            autoApprove
              ? 'Tool permissions are auto-approved — click to require manual approval'
              : 'Tool permissions require manual approval — click to auto-approve'
          }
          style={{
            fontSize: 11,
            padding: '3px 8px',
            borderRadius: 4,
            cursor: 'pointer',
            background: autoApprove ? '#1f2a1f' : '#2a261f',
            border: `1px solid ${autoApprove ? '#2a4a2a' : '#4a402a'}`,
            color: autoApprove ? '#aea' : '#eca'
          }}
        >
          {autoApprove ? 'Auto-approve' : 'Manual approve'}
        </button>
        <button
          type="button"
          onClick={onReset}
          title="Kill the agent session and clear the conversation"
          style={{
            fontSize: 11,
            padding: '3px 8px',
            background: '#2a1f1f',
            border: '1px solid #4a2a2a',
            color: '#eaa',
            borderRadius: 4,
            cursor: 'pointer'
          }}
          disabled={messages.length === 0 && !busy}
        >
          Reset
        </button>
      </header>

      <div style={{ position: 'relative', flex: 1, minHeight: 0 }}>
        <div
          ref={scrollRef}
          onScroll={onScroll}
          style={{ position: 'absolute', inset: 0, overflowY: 'auto', padding: 12, scrollbarGutter: 'stable' }}
        >
          {messages.length === 0 && (
            <p style={{ opacity: 0.5 }}>
              Open a site in the browser and ask me to analyze its API traffic, or describe an
              action you just performed and I&apos;ll dig into the requests it triggered.
            </p>
          )}
          {messages.map((m) => (
            <div key={m.id} style={{ marginBottom: 16 }}>
              <div style={{ fontSize: 11, opacity: 0.6, marginBottom: 4 }}>{m.role}</div>
              {groupParts(m.parts as unknown as AnyPart[]).map((g) =>
                g.kind === 'text' ? (
                  <Markdown key={g.key} text={(g.part as { text: string }).text} />
                ) : (
                  <WorkGroup key={g.key} parts={g.parts} />
                )
              )}
            </div>
          ))}
          {waiting && <Thinking />}
        </div>

        {!autoScroll && (
          <button
            type="button"
            onClick={() => {
              setAutoScroll(true)
              scrollToBottom('smooth')
            }}
            style={{
              position: 'absolute',
              right: 12,
              bottom: 12,
              padding: '6px 10px',
              borderRadius: 16,
              border: '1px solid #444',
              background: '#1f1f1f',
              color: '#eee',
              fontSize: 12,
              cursor: 'pointer',
              boxShadow: '0 2px 8px rgba(0,0,0,0.4)'
            }}
          >
            ↓ jump to latest
          </button>
        )}
      </div>

      <form onSubmit={onSubmit} style={{ display: 'flex', gap: 8, padding: 12, borderTop: '1px solid #333', alignItems: 'center' }}>
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="Type a message…"
          style={{ flex: 1, padding: '6px 10px' }}
          disabled={busy}
        />
        {busy && <Spinner />}
        {busy ? (
          <button type="button" onClick={() => stop()} style={{ background: '#3a1f1f', borderColor: '#5a2a2a' }}>
            Stop
          </button>
        ) : (
          <button type="submit" disabled={!input.trim()}>
            Send
          </button>
        )}
      </form>
    </div>
  )
}
