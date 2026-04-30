import { useEffect, useState } from 'react'

import { useChatDraft } from '@/stores/chat-draft'
import { tryPretty } from '@/lib/format-json'

import type { StoredRequestSummary } from '../../../../preload'

type Tab = 'overview' | 'headers' | 'body'

export function TrafficDetailDrawer({
  requestId,
  onClose
}: {
  requestId: string
  onClose: () => void
}) {
  const [data, setData] = useState<StoredRequestSummary | null>(null)
  const [tab, setTab] = useState<Tab>('overview')
  const pushDraft = useChatDraft((s) => s.push)

  useEffect(() => {
    let cancelled = false
    setData(null)
    setTab('overview')
    void window.rev.traffic.get(requestId).then((d) => {
      if (!cancelled) setData(d)
    })
    return () => {
      cancelled = true
    }
  }, [requestId])

  const onSendToChat = () => {
    if (!data) return
    pushDraft(
      `이 요청을 분석해줘: ${data.method} ${data.url} (id: ${data.requestId}${
        data.status ? `, status: ${data.status}` : ''
      })\n\n질문: `
    )
  }

  return (
    <div
      style={{
        width: '100%',
        height: '100%',
        display: 'flex',
        flexDirection: 'column',
        background: '#0f0f0f'
      }}
    >
      <header
        style={{
          padding: '8px 12px',
          borderBottom: '1px solid #2a2a2a',
          display: 'flex',
          gap: 8,
          alignItems: 'center',
          fontSize: 12
        }}
      >
        <strong>Request</strong>
        <button onClick={onSendToChat} style={{ fontSize: 11 }}>
          Send to chat
        </button>
        <button onClick={onClose} style={{ marginLeft: 'auto', fontSize: 11 }}>
          ✕
        </button>
      </header>

      <nav
        style={{
          display: 'flex',
          borderBottom: '1px solid #2a2a2a',
          fontSize: 12
        }}
      >
        {(['overview', 'headers', 'body'] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            style={{
              flex: 1,
              padding: '6px 8px',
              border: 'none',
              borderRadius: 0,
              background: tab === t ? '#1a1a1a' : 'transparent',
              color: tab === t ? '#fff' : '#999',
              borderBottom: tab === t ? '2px solid #4a8fff' : '2px solid transparent'
            }}
          >
            {t}
          </button>
        ))}
      </nav>

      <div style={{ flex: 1, overflowY: 'auto', padding: 12, fontSize: 12 }}>
        {!data && <p style={{ opacity: 0.5 }}>Loading…</p>}
        {data && tab === 'overview' && <Overview data={data} />}
        {data && tab === 'headers' && <Headers data={data} />}
        {data && tab === 'body' && <Body data={data} />}
      </div>
    </div>
  )
}

function Field({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div style={{ marginBottom: 8 }}>
      <div style={{ fontSize: 11, opacity: 0.5, marginBottom: 2 }}>{label}</div>
      <div style={{ wordBreak: 'break-all' }}>{value}</div>
    </div>
  )
}

function Overview({ data }: { data: StoredRequestSummary }) {
  const elapsed =
    data.completedAt && data.startedAt ? `${data.completedAt - data.startedAt}ms` : '—'
  return (
    <div>
      <Field label="Method" value={data.method} />
      <Field label="URL" value={data.url} />
      <Field label="Host" value={data.host} />
      <Field label="Type" value={data.resourceType} />
      <Field label="Status" value={data.status ?? '—'} />
      <Field label="MIME" value={data.mimeType ?? '—'} />
      <Field
        label="Size"
        value={data.encodedDataLength != null ? `${data.encodedDataLength} bytes` : '—'}
      />
      <Field label="Elapsed" value={elapsed} />
      <Field label="requestId" value={<code>{data.requestId}</code>} />
    </div>
  )
}

function HeaderTable({ headers }: { headers: Record<string, string> | undefined }) {
  if (!headers || Object.keys(headers).length === 0) {
    return <p style={{ opacity: 0.5 }}>—</p>
  }
  return (
    <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 11 }}>
      <tbody>
        {Object.entries(headers).map(([k, v]) => (
          <tr key={k} style={{ borderBottom: '1px solid #1a1a1a' }}>
            <td style={{ padding: '3px 6px', opacity: 0.7, verticalAlign: 'top', width: 130 }}>
              {k}
            </td>
            <td style={{ padding: '3px 6px', wordBreak: 'break-all', fontFamily: 'ui-monospace, monospace' }}>
              {v}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function Headers({ data }: { data: StoredRequestSummary }) {
  return (
    <div>
      <h4 style={{ margin: '0 0 6px', fontSize: 12 }}>Request</h4>
      <HeaderTable headers={data.requestHeaders} />
      <h4 style={{ margin: '14px 0 6px', fontSize: 12 }}>Response</h4>
      <HeaderTable headers={data.responseHeaders} />
    </div>
  )
}

function BodyBlock({ label, body }: { label: string; body?: string }) {
  return (
    <div style={{ marginBottom: 14 }}>
      <h4 style={{ margin: '0 0 6px', fontSize: 12 }}>{label}</h4>
      {body ? (
        <pre
          style={{
            margin: 0,
            padding: 10,
            background: '#0a0a0a',
            border: '1px solid #1a1a1a',
            borderRadius: 4,
            maxHeight: 360,
            overflow: 'auto',
            fontSize: 11,
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-word'
          }}
        >
          {tryPretty(body)}
        </pre>
      ) : (
        <p style={{ opacity: 0.5, margin: 0 }}>—</p>
      )}
    </div>
  )
}

function Body({ data }: { data: StoredRequestSummary }) {
  return (
    <div>
      <BodyBlock label="Request body" body={data.requestPostData} />
      {data.responseBodyError ? (
        <div>
          <h4 style={{ margin: '0 0 6px', fontSize: 12 }}>Response body</h4>
          <p style={{ color: '#ff7676', fontSize: 11 }}>error: {data.responseBodyError}</p>
        </div>
      ) : data.responseBodyBase64 ? (
        <div>
          <h4 style={{ margin: '0 0 6px', fontSize: 12 }}>Response body</h4>
          <p style={{ opacity: 0.5, fontSize: 11 }}>
            (base64-encoded binary, {data.responseBody?.length ?? 0} chars)
          </p>
        </div>
      ) : (
        <BodyBlock label="Response body" body={data.responseBody} />
      )}
    </div>
  )
}
