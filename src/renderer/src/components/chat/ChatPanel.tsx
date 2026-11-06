import { useChat } from '@ai-sdk/react'
import { memo, useEffect, useMemo, useRef, useState, type FormEvent } from 'react'

import { ACPChatTransport } from '@/ai/acp-transport'
import { useAcpAutoApprove, setAcpAutoApprove } from '@/ai/acp-permission'
import { ACP_AGENTS, type ACPAgentID, type CatalogModel } from '@/constants'
import { useAgentModels } from '@/stores/model-catalog'
import { ChipSelect } from './ChipSelect'
import { ModelMenu, type MenuModel } from './ModelMenu'
import { Markdown } from './Markdown'
import { ChatHistoryMenu } from './ChatHistoryMenu'
import { formatOutput } from '@/lib/format-json'
import { useChatDraft } from '@/stores/chat-draft'
import { newConversationId, useChatHistory } from '@/stores/chat-history'
import { readAgentChoice, useAgentChoice } from '@/stores/agent-choice'

import type { UIMessage } from 'ai'

import { useT } from '@/stores/i18n'
import { useToolProfileStore, type ToolProfileId } from '@/stores/tool-profile'
import type { TKey } from '@/locales/en'
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
  const tr = useT()
  const [open, setOpen] = useState(false)
  const name = part.title || part.toolName || part.type.replace(/^tool-/, '')
  const state = part.state ?? (part.output != null ? 'done' : part.errorText ? 'error' : '…')
  const stateColor =
    state === 'error' ? 'var(--status-error)' : state === 'done' || state === 'output-available' ? 'var(--status-ok)' : 'var(--text-dim)'
  return (
    <div
      style={{
        background: 'var(--bg-bar)',
        border: '1px solid var(--border)',
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
          color: 'var(--text-2)',
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
              <summary style={{ cursor: 'pointer', opacity: 0.7 }}>{tr('chat.input')}</summary>
              <pre style={preStyle}>{JSON.stringify(part.input, null, 2)}</pre>
            </details>
          )}
          {part.output != null && (
            <details open style={{ marginTop: 4 }}>
              <summary style={{ cursor: 'pointer', opacity: 0.7 }}>{tr('chat.output')}</summary>
              <pre style={preStyle}>{formatOutput(part.output)}</pre>
            </details>
          )}
          {part.errorText && (
            <pre style={{ ...preStyle, color: 'var(--status-error)' }}>{part.errorText}</pre>
          )}
        </div>
      )}
    </div>
  )
}

interface AnyPart {
  type: string
  text?: string
  data?: { kind?: string; title?: string; text?: string }
}

// A policy-refusal (or similar) notice emitted by the transport as a
// `data-notice` part. Rendered as a distinct banner, never blended into the
// agent's markdown answer.
function NoticeBanner({ part }: { part: AnyPart }) {
  const title = part.data?.title
  const text = part.data?.text ?? ''
  const accent = part.data?.kind === 'refusal' ? 'var(--status-error)' : 'var(--status-warn)'
  return (
    <div
      data-testid="chat-notice"
      role="alert"
      style={{
        display: 'flex',
        gap: 10,
        alignItems: 'flex-start',
        margin: '8px 0',
        padding: '12px 14px',
        fontSize: 12,
        lineHeight: 1.55,
        background: `color-mix(in srgb, ${accent} 8%, var(--surface))`,
        borderRadius: 8,
        color: 'var(--text-2)'
      }}
    >
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke={accent}
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
        style={{ flexShrink: 0, marginTop: 1 }}
      >
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
      <div>
        {title && (
          <div style={{ fontWeight: 600, color: accent, marginBottom: 3 }}>{title}</div>
        )}
        <div style={{ color: 'var(--text-dim)' }}>{text}</div>
      </div>
    </div>
  )
}

type Group =
  | { kind: 'text'; part: AnyPart; key: number }
  | { kind: 'notice'; part: AnyPart; key: number }
  | { kind: 'work'; parts: AnyPart[]; key: number }

function groupParts(parts: AnyPart[]): Group[] {
  const groups: Group[] = []
  let buf: AnyPart[] = []
  const flush = (i: number) => {
    if (buf.length) {
      groups.push({ kind: 'work', parts: buf, key: i - buf.length })
      buf = []
    }
  }
  parts.forEach((p, i) => {
    if (p.type === 'text') {
      flush(i)
      groups.push({ kind: 'text', part: p, key: i })
    } else if (p.type === 'data-notice') {
      flush(i)
      groups.push({ kind: 'notice', part: p, key: i })
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

  // data-tools: the tool names in this group, for E2E assertions (e2e/).
  const toolNames = parts
    .filter((p) => p.type.startsWith('tool-'))
    .map((p) => {
      const t = p as ToolPart
      return t.title || t.toolName || p.type.replace(/^tool-/, '')
    })

  return (
    <div style={{ margin: '4px 0' }} data-testid="chat-work" data-tools={JSON.stringify(toolNames)}>
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
          border: '1px solid var(--border)',
          borderRadius: 12,
          color: 'var(--text-dim)',
          cursor: 'pointer'
        }}
      >
        <span>{open ? '▾' : '▸'}</span>
        <span>{summary || 'work'}</span>
        <span style={{ opacity: 0.5 }}>· {open ? 'hide' : 'show more'}</span>
      </button>
      {open && (
        <div style={{ marginTop: 6, paddingLeft: 8, borderLeft: '2px solid var(--border)' }}>
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

// 메시지 1개를 렌더한다. React.memo로 감싸 message 참조가 바뀐 메시지만 다시
// 그리게 한다. AI SDK는 완료된 메시지의 객체 참조를 유지하고 스트리밍 중인
// 마지막 메시지만 새 객체로 교체하므로, 토큰마다 과거 메시지 전체를
// react-markdown으로 재파싱하던 O(N²) 비용이 스트리밍 중인 1개로 줄어든다.
interface ChatMessageLike {
  id: string
  role: string
  parts: AnyPart[]
}

const MessageItem = memo(function MessageItem({ message }: { message: ChatMessageLike }) {
  return (
    <div
      style={{ marginBottom: 16 }}
      className={message.role === 'user' ? 'chat-user-bubble' : undefined}
      data-testid="chat-message"
      data-role={message.role}
    >
      {groupParts(message.parts).map((g) =>
        g.kind === 'text' ? (
          <Markdown key={g.key} text={(g.part as { text: string }).text} />
        ) : g.kind === 'notice' ? (
          <NoticeBanner key={g.key} part={g.part} />
        ) : (
          <WorkGroup key={g.key} parts={g.parts} />
        )
      )}
    </div>
  )
})

const preStyle: React.CSSProperties = {
  background: 'var(--bg)',
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

type ModelEntry = MenuModel

const TOOL_PROFILE_LABEL: Record<ToolProfileId, TKey> = {
  browse: 'tools.profile.browse',
  reverse: 'tools.profile.reverse',
  probe: 'tools.profile.probe'
}
const TOOL_PROFILE_TIP: Record<ToolProfileId, TKey> = {
  browse: 'tools.profile.tip.browse',
  reverse: 'tools.profile.tip.reverse',
  probe: 'tools.profile.tip.probe'
}
const TOOL_PROFILE_ORDER: readonly ToolProfileId[] = ['browse', 'reverse', 'probe']

// Autonomous scan bar above the chat input: URL + page budget + Start. Start
export interface ChatTabMeta {
  title: string
  agentId: ACPAgentID
  busy: boolean
}

/**
 * One chat session. ChatTabs mounts one per tab and hides the inactive ones, so
 * each keeps its own agent process and stream running.
 */
export function ChatPanel({
  tabId = 'main',
  active = true,
  restore = true,
  onMeta,
  initialAgent
}: {
  /** Agent this tab was opened with (fixed for the tab's life). */
  initialAgent?: ACPAgentID
  tabId?: string
  active?: boolean
  /** Reopen the most recent saved conversation (only the first tab does). */
  restore?: boolean
  onMeta?: (tabId: string, meta: ChatTabMeta) => void
} = {}) {
  const tr = useT()
  // Restore the most recently used conversation on launch so a reload/restart
  // doesn't drop the last chat. The persist store hydrates synchronously from
  // localStorage, so getState() already has the saved conversations here.
  const restored = restore ? (useChatHistory.getState().conversations[0] ?? null) : null

  // Falls back to the agent picked during first-run onboarding before the
  // hardcoded default, so a machine without claude-agent-acp doesn't land on a
  // dead selection.
  const onboarded = readAgentChoice()
  const [agentId, setAgentId] = useState<ACPAgentID>(
    initialAgent ?? restored?.agentId ?? onboarded?.id ?? 'claude-code'
  )
  // Absolute path returned by detectAgents(). When set, we pass it to the
  // transport instead of the bare bin name so spawn doesn't depend on the
  // Electron child process's PATH (notably broken on Windows for .cmd shims).
  const [input, setInput] = useState('')
  const [autoScroll, setAutoScroll] = useState(true)
  const [models, setModels] = useState<ModelEntry[]>([])
  const [currentModel, setCurrentModel] = useState<string | null>(null)
  // Model chosen in the unified picker before the session exposes its live
  // list; reconciled against that list (by id or name) once it loads.
  const [pendingModel, setPendingModel] = useState<CatalogModel | null>(null)
  const [selectedModelName, setSelectedModelName] = useState<string | null>(null)
  const [effort, setEffort] = useState<string | null>(null)
  const [fastMode, setFastMode] = useState(false)
  // Set when a model switch is rejected (e.g. an ACP agent that doesn't
  // implement the unstable model-selection extension), so the choice doesn't
  // silently no-op in the UI.
  const [modelError, setModelError] = useState(false)
  const scrollRef = useRef<HTMLDivElement>(null)

  // --- Conversation history wiring ---------------------------------------
  // `chatKey` drives useChat's Chat lifecycle: it changes only when we want a
  // fresh Chat (agent switch, "New chat", or opening a saved conversation via
  // sessionSeq). The storage id lives in currentIdRef so the draft→saved
  // transition doesn't churn chatKey and lose the in-flight message.
  const [sessionSeq, setSessionSeq] = useState(0)
  const [currentId, setCurrentId] = useState<string | null>(restored?.id ?? null)
  const [seedMessages, setSeedMessages] = useState<UIMessage[]>(restored?.messages ?? [])
  const currentIdRef = useRef<string | null>(restored?.id ?? null)
  const saveConversation = useChatHistory((s) => s.save)

  // One transport per agent identity. The binary path is resolved in the main
  // process at spawn time, so nothing here depends on async PATH detection.
  const transport = useMemo(() => {
    const base = ACP_AGENTS.find((a) => a.id === agentId)!
    return new ACPChatTransport({ agentDef: base })
  }, [agentId])

  // A pick made in the first-run onboarding modal switches the live chat, so
  // the user doesn't have to reopen the agent picker right after choosing.
  const onboardChoice = useAgentChoice((s) => s.choice)
  useEffect(() => {
    if (!onboardChoice) return
    if (onboardChoice.id === agentId) return
    currentIdRef.current = null
    setCurrentId(null)
    setSeedMessages([])
    setAgentId(onboardChoice.id)
  }, [onboardChoice, agentId])

  // @ai-sdk/react's useChat only recreates its internal Chat when `id`
  // changes — a fresh `transport` prop alone is ignored. Keying on agentId
  // (switch agents → fresh chat + torn-down session) and sessionSeq (new /
  // opened conversation) covers every case where we intend to reseed; resolving
  // the binary path must NOT reset the chat, so it stays out of the key.
  const chatKey = `${tabId}::${agentId}::${sessionSeq}`

  // Tear down the previous ACP session when the agent (and thus transport)
  // changes, so we don't leak the old child process.
  useEffect(() => {
    return () => {
      void transport.reset()
    }
  }, [transport])

  const { messages, sendMessage, status, stop, resumeStream, error: chatError } = useChat({
    id: chatKey,
    transport,
    messages: seedMessages
  })
  const busy = status === 'streaming' || status === 'submitted'

  const firstUserText = (() => {
    const first = messages.find((m) => m.role === 'user')
    const part = first?.parts.find((p) => p.type === 'text') as { text?: string } | undefined
    return part?.text?.trim().split('\n')[0] ?? ''
  })()
  useEffect(() => {
    onMeta?.(tabId, {
      title: firstUserText || (ACP_AGENTS.find((a) => a.id === agentId)?.name ?? agentId),
      agentId,
      busy
    })
  }, [onMeta, tabId, firstUserText, agentId, busy])

  // A turn the agent starts on its own (a background task it said it would
  // wait for just finished) streams into the chat without a new user message.
  // It is attached once the chat is idle, so it never races a live turn.
  const [backgroundSeq, setBackgroundSeq] = useState(0)
  useEffect(() => transport.watchBackground(() => setBackgroundSeq((n) => n + 1)), [transport])
  useEffect(() => {
    if (backgroundSeq === 0 || busy || !transport.hasPendingBackground()) return
    void resumeStream()
  }, [backgroundSeq, busy, transport, resumeStream])

  // Persist the conversation whenever a turn settles. Streaming updates are
  // skipped (status !== 'ready'), so this writes once per completed turn.
  useEffect(() => {
    if (status !== 'ready') return
    const id = currentIdRef.current
    if (!id || messages.length === 0) return
    saveConversation({ id, agentId, messages: messages as UIMessage[] })
  }, [status, messages, agentId, saveConversation])

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
      if (!state) {
        setModels([])
        setCurrentModel(null)
        return
      }
      setModels(state.availableModels)
      setCurrentModel(state.currentModelId)
      setEffort(state.effort ?? null)
      setFastMode(state.fastMode === true)
    })
    return () => {
      cancelled = true
    }
  }, [status, transport])

  const onChangeOption = async (key: 'effort' | 'fastMode', value: string | boolean) => {
    const sid = transport.getSessionId()
    if (!sid) return
    try {
      await window.rev.acp.setOption(sid, key, value)
      const state = await window.rev.acp.modelState(sid)
      setEffort(state?.effort ?? null)
      setFastMode(state?.fastMode === true)
      setModelError(false)
    } catch (e) {
      console.error('[acp] setOption failed', e)
      setModelError(true)
    }
  }

  const onChangeModel = async (modelId: string) => {
    const sid = transport.getSessionId()
    if (!sid) return
    setCurrentModel(modelId)
    setModelError(false)
    try {
      await window.rev.acp.setModel(sid, modelId)
      const state = await window.rev.acp.modelState(sid)
      setEffort(state?.effort ?? null)
      setFastMode(state?.fastMode === true)
    } catch (e) {
      console.error('[acp] setModel failed', e)
      setModelError(true)
    }
  }

  // Apply a picker choice once the session is live. Subscription ACP agents
  // (e.g. Claude Code) sometimes don't advertise a model list at all; in that
  // case trust the catalog id directly instead of dropping the choice — that
  // empty-list no-op was why switching "didn't work". When a live list IS
  // offered, reconcile by id or name (the catalog id is best-effort there).
  useEffect(() => {
    if (!pendingModel || pendingModel.agentId !== agentId) return
    if (status !== 'ready' || !transport.getSessionId()) return
    if (models.length > 0) {
      const wanted = pendingModel.name.toLowerCase()
      const live =
        models.find((m) => m.modelId === pendingModel.modelId) ??
        models.find((m) => m.name.toLowerCase() === wanted)
      if (live) {
        if (live.modelId !== currentModel) void onChangeModel(live.modelId)
        setSelectedModelName(live.name)
      }
      setPendingModel(null)
    } else if (agentId === 'claude-code' || agentId === 'codex') {
      // Subscription ACP agent advertised no model list: send the catalog id
      // straight through instead of dropping the choice. If the agent rejects
      // it, onChangeModel surfaces modelError rather than failing silently.
      if (pendingModel.modelId !== currentModel) void onChangeModel(pendingModel.modelId)
      setSelectedModelName(pendingModel.name)
      setPendingModel(null)
    }
    // else: a key-provider list is still loading — keep pendingModel and
    // reconcile when `models` populates on the next run.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingModel, models, agentId, currentModel, status, transport])
  const waiting = status === 'submitted'
  const autoApprove = useAcpAutoApprove()
  // What the composer's model chip shows: the user's pick, else the agent's live model.
  const modelLabel =
    selectedModelName ?? models.find((m) => m.modelId === currentModel)?.name ?? currentModel ?? null
  // Before the session lists its own models, show the agent's live CLI catalog.
  const catalogModels = useAgentModels(agentId)
  const toolProfile = useToolProfileStore((s) => s.profile)
  const setToolProfile = useToolProfileStore((s) => s.setProfile)
  const draftPending = useChatDraft((s) => s.pending)
  const consumeDraft = useChatDraft((s) => s.consume)

  // Start a fresh, empty conversation: kill the current agent session and
  // reseed useChat with an empty message list via a new sessionSeq.
  const onNewChat = async () => {
    if (busy) {
      try {
        stop()
      } catch {}
    }
    await transport.reset()
    currentIdRef.current = null
    setCurrentId(null)
    setSeedMessages([])
    setSessionSeq((n) => n + 1)
    setInput('')
    setAutoScroll(true)
  }

  // Load a saved conversation into the panel. Note: this restores the visible
  // transcript only — the agent has no server-side memory of it, so the next
  // message spawns a fresh session (system prompt re-sent).
  const openConversation = async (id: string) => {
    const conv = useChatHistory.getState().conversations.find((c) => c.id === id)
    if (!conv) return
    if (busy) {
      try {
        stop()
      } catch {}
    }
    await transport.reset()
    currentIdRef.current = conv.id
    setCurrentId(conv.id)
    setSeedMessages(conv.messages)
    setAgentId(conv.agentId)
    setSessionSeq((n) => n + 1)
    setInput('')
    setAutoScroll(true)
  }

  // Only the visible tab takes text sent to the chat from elsewhere in the app.
  useEffect(() => {
    if (!active || draftPending == null) return
    const text = consumeDraft()
    if (text == null) return
    setInput((prev) => (prev ? `${prev}\n${text}` : text))
  }, [active, draftPending, consumeDraft])

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

  const submit = () => {
    const text = input.trim()
    if (!text || busy) return
    // Assign a storage id for the draft on first send. This does NOT change
    // chatKey, so the in-flight message isn't lost; the save effect picks it
    // up from currentIdRef once the turn completes.
    if (!currentIdRef.current) {
      const id = newConversationId()
      currentIdRef.current = id
      setCurrentId(id)
    }
    setInput('')
    setAutoScroll(true)
    void sendMessage({ text })
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    submit()
  }


  // Auto-grow the input up to 4 lines; beyond that it scrolls.
  const inputRef = useRef<HTMLTextAreaElement>(null)
  const INPUT_MAX_HEIGHT = 20 * 4 + 14 // 4 lines × line-height + padding + border
  useEffect(() => {
    const el = inputRef.current
    if (!el) return
    el.style.height = 'auto'
    // border-box: scrollHeight excludes the border, so add it back or the box
    // is permanently 2px short and shows a scrollbar even on a single line.
    const chrome = el.offsetHeight - el.clientHeight
    const needed = el.scrollHeight + chrome
    el.style.height = `${Math.min(needed, INPUT_MAX_HEIGHT)}px`
    el.style.overflowY = needed > INPUT_MAX_HEIGHT ? 'auto' : 'hidden'
  }, [input])

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', borderLeft: '1px solid var(--border-2)' }}>
      <header style={{ padding: '8px 12px', borderBottom: '1px solid var(--border-2)', display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
        {modelError && (
          <span
            title={tr('chat.modelSwitchFailed')}
            style={{ color: 'var(--status-error)', fontSize: 12, whiteSpace: 'nowrap' }}
          >
            ⚠ {tr('chat.modelSwitchFailed')}
          </span>
        )}
        <span style={{ marginLeft: 'auto', opacity: 0.6, fontSize: 12 }}>{status}</span>
        <ChatHistoryMenu currentId={currentId} disabled={busy} onOpen={openConversation} />
        <button
          type="button"
          onClick={onNewChat}
          title={tr('chat.newConversation')}
          data-testid="chat-new"
          style={{
            fontSize: 11,
            padding: '3px 8px',
            background: 'var(--chip-info-bg)',
            border: '1px solid var(--chip-info-border)',
            color: 'var(--chip-info-text)',
            borderRadius: 4,
            cursor: 'pointer'
          }}
          disabled={messages.length === 0 && !busy}
        >
          + New
        </button>
      </header>

      <div style={{ position: 'relative', flex: 1, minHeight: 0 }}>
        <div
          ref={scrollRef}
          onScroll={onScroll}
          className="chat-log"
          style={{ position: 'absolute', inset: 0, overflowY: 'auto', padding: '14px 16px', scrollbarGutter: 'stable' }}
        >
          {messages.length === 0 && (
            <p style={{ opacity: 0.5 }}>
              Open a site in the browser and ask me to analyze its API traffic, or describe an
              action you just performed and I&apos;ll dig into the requests it triggered.
            </p>
          )}
          {messages.map((m) => (
            <MessageItem key={m.id} message={m as unknown as ChatMessageLike} />
          ))}
          {waiting && <Thinking />}
          {chatError && (
            <pre
              style={{
                color: 'var(--status-error)',
                whiteSpace: 'pre-wrap',
                fontSize: 12,
                margin: '4px 0'
              }}
            >
              ⚠ {tr('chat.turnFailed')}: {chatError.message}
            </pre>
          )}
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
              border: '1px solid var(--border-3)',
              background: 'var(--surface)',
              color: 'var(--text)',
              fontSize: 12,
              cursor: 'pointer',
              boxShadow: '0 2px 8px rgba(0,0,0,0.4)'
            }}
          >
            ↓ jump to latest
          </button>
        )}
      </div>

      <form
        onSubmit={onSubmit}
        data-testid="chat-form"
        data-status={status}
        className="composer"
      >
        <textarea
          data-testid="chat-input"
          ref={inputRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            // Enter sends, Shift+Enter inserts a newline. Skip while the IME
            // is composing (e.g. Korean) so Enter doesn't send mid-composition.
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault()
              submit()
            }
          }}
          placeholder={tr('chat.placeholder')}
          rows={1}
          style={{
            resize: 'none',
            maxHeight: INPUT_MAX_HEIGHT,
            // The auto-grow effect flips this to 'auto' once it overflows.
            overflowY: 'hidden',
            display: 'block',
            fontFamily: 'inherit',
            fontSize: 14,
            lineHeight: '20px',
            boxSizing: 'border-box'
          }}
          disabled={busy}
        />
        <div className="composer-row">
          <ChipSelect
            testId="chat-permission"
            title={tr('chat.perm.label')}
            value={autoApprove ? 'auto' : 'manual'}
            onChange={(v) => setAcpAutoApprove(v === 'auto')}
            icon={
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
              </svg>
            }
            options={[
              { value: 'auto', label: tr('chat.perm.auto'), hint: tr('chat.perm.auto.hint') },
              { value: 'manual', label: tr('chat.perm.manual'), hint: tr('chat.perm.manual.hint') }
            ]}
          />
          <ChipSelect
            testId="tool-profile"
            title={tr('tools.profile.label')}
            value={toolProfile}
            onChange={(v) => void setToolProfile(v)}
            icon={
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
              </svg>
            }
            options={TOOL_PROFILE_ORDER.map((id) => ({
              value: id,
              label: tr(TOOL_PROFILE_LABEL[id]),
              hint: tr(TOOL_PROFILE_TIP[id]),
              testId: `tool-profile-${id}`
            }))}
          />
          <span style={{ flex: 1 }} />
          <ModelMenu
            agentId={agentId}
            // Before the session exists, offer the static catalog so a model can be picked up front.
            models={models.length ? models : catalogModels}
            currentModelId={currentModel ?? pendingModel?.modelId ?? null}
            label={modelLabel ?? (ACP_AGENTS.find((a) => a.id === agentId)?.name ?? agentId)}
            effort={effort}
            fastMode={fastMode}
            live={!!transport.getSessionId()}
            disabled={busy}
            onModel={(m) => {
              setSelectedModelName(m.name)
              setPendingModel({ agentId, modelId: m.modelId, name: m.name })
            }}
            onEffort={(level) => void onChangeOption('effort', level)}
            onFastMode={(on) => void onChangeOption('fastMode', on)}
          />
          {busy && <Spinner />}
          {busy ? (
            <button type="button" className="composer-send" onClick={() => stop()} title={tr('chat.stop')} aria-label={tr('chat.stop')}>
              <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <rect x="5" y="5" width="14" height="14" rx="2" />
              </svg>
            </button>
          ) : (
            <button type="submit" className="composer-send" disabled={!input.trim()} title={tr('chat.send')} aria-label={tr('chat.send')}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M12 19V5M5 12l7-7 7 7" />
              </svg>
            </button>
          )}
        </div>
      </form>
    </div>
  )
}
