import { useEffect, useMemo, useRef, useState } from 'react'

import type { WorkflowEditorProps } from '../core/registry'
import { resolvePipeline } from './resolve'
import { nodeId, type PipeCond, type PipeNode, type PipelineData } from './types'

interface ToolInfo {
  name: string
  description?: string
  inputSchema: unknown
}

interface PipeProgress {
  nodeId: string
  tool?: string
  status: 'running' | 'done' | 'error' | 'branch'
  output?: string
  error?: string
  taken?: 'then' | 'else'
}

type ProgressMap = Record<string, PipeProgress>

const CONTROL_LABEL = { color: 'var(--text-dim)', fontSize: 11 } as const
const MONO_INPUT = {
  fontFamily: 'ui-monospace, monospace',
  fontSize: 11,
  padding: 6,
  resize: 'vertical' as const,
  background: 'var(--surface-2)',
  color: 'var(--text)',
  border: '1px solid var(--border-2)',
  borderRadius: 4
}

function newTool(tools: ToolInfo[]): PipeNode {
  return { id: nodeId(), type: 'tool', tool: tools[0]?.name ?? '', input: '{}' }
}
function newIf(): PipeNode {
  return { id: nodeId(), type: 'if', cond: { on: 'output', op: 'contains', value: '' }, then: [], else: [] }
}

// A controlled list of nodes. Each nested branch is just another NodeList
// controlled by its parent, so tree edits are plain array maps — no id walking.
function NodeList({
  nodes,
  onChange,
  tools,
  progress
}: {
  nodes: PipeNode[]
  onChange: (nodes: PipeNode[]) => void
  tools: ToolInfo[]
  progress: ProgressMap
}): React.ReactElement {
  const update = (i: number, node: PipeNode): void => onChange(nodes.map((n, j) => (j === i ? node : n)))
  const remove = (i: number): void => onChange(nodes.filter((_, j) => j !== i))
  const move = (i: number, dir: -1 | 1): void => {
    const j = i + dir
    if (j < 0 || j >= nodes.length) return
    const next = [...nodes]
    ;[next[i], next[j]] = [next[j], next[i]]
    onChange(next)
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      {nodes.map((n, i) => (
        <NodeCard
          key={n.id}
          node={n}
          tools={tools}
          progress={progress}
          canUp={i > 0}
          canDown={i < nodes.length - 1}
          onChange={(node) => update(i, node)}
          onRemove={() => remove(i)}
          onMove={(dir) => move(i, dir)}
        />
      ))}
      <div style={{ display: 'flex', gap: 6 }}>
        <button type="button" onClick={() => onChange([...nodes, newTool(tools)])} disabled={tools.length === 0}>
          + Tool
        </button>
        <button type="button" onClick={() => onChange([...nodes, newIf()])}>
          + If
        </button>
      </div>
    </div>
  )
}

function NodeCard({
  node,
  onChange,
  onRemove,
  onMove,
  canUp,
  canDown,
  tools,
  progress
}: {
  node: PipeNode
  onChange: (node: PipeNode) => void
  onRemove: () => void
  onMove: (dir: -1 | 1) => void
  canUp: boolean
  canDown: boolean
  tools: ToolInfo[]
  progress: ProgressMap
}): React.ReactElement {
  const prog = progress[node.id]
  const border =
    prog?.status === 'running'
      ? 'var(--accent)'
      : prog?.status === 'error'
        ? '#e06c6c'
        : 'var(--border)'

  const header = (
    <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
      <span style={{ ...CONTROL_LABEL, textTransform: 'uppercase', letterSpacing: 0.4 }}>
        {node.type === 'if' ? 'If' : 'Tool'}
      </span>
      <div style={{ marginLeft: 'auto', display: 'flex', gap: 4 }}>
        <button type="button" onClick={() => onMove(-1)} disabled={!canUp} title="Move up">↑</button>
        <button type="button" onClick={() => onMove(1)} disabled={!canDown} title="Move down">↓</button>
        <button type="button" onClick={onRemove} title="Remove">✕</button>
      </div>
    </div>
  )

  if (node.type === 'tool') {
    return (
      <div style={{ border: `1px solid ${border}`, borderRadius: 6, padding: 8, background: 'var(--surface)', display: 'flex', flexDirection: 'column', gap: 6 }}>
        {header}
        <select
          value={node.tool}
          onChange={(e) => onChange({ ...node, tool: e.target.value })}
          style={{ height: 26, padding: '0 6px' }}
        >
          {!node.tool && <option value="">Select a tool…</option>}
          {tools.map((t) => (
            <option key={t.name} value={t.name}>{t.name}</option>
          ))}
        </select>
        <textarea
          value={node.input}
          onChange={(e) => onChange({ ...node, input: e.target.value })}
          spellCheck={false}
          rows={2}
          placeholder='{ "url": "https://..." }'
          style={MONO_INPUT}
        />
        {prog && <ProgressLine prog={prog} />}
      </div>
    )
  }

  // if-node: condition + two nested branches
  const cond = node.cond
  const setCond = (patch: Partial<PipeCond>): void => onChange({ ...node, cond: { ...cond, ...patch } })
  return (
    <div style={{ border: `1px solid ${border}`, borderRadius: 6, padding: 8, background: 'var(--surface)', display: 'flex', flexDirection: 'column', gap: 6 }}>
      {header}
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4, alignItems: 'center' }}>
        <span style={CONTROL_LABEL}>if last</span>
        <select value={cond.on} onChange={(e) => setCond({ on: e.target.value as PipeCond['on'] })} style={{ height: 24 }}>
          <option value="output">output</option>
          <option value="error">error</option>
        </select>
        <select value={cond.op} onChange={(e) => setCond({ op: e.target.value as PipeCond['op'] })} style={{ height: 24 }}>
          <option value="contains">contains</option>
          <option value="equals">equals</option>
          <option value="matches">matches</option>
          <option value="always">always</option>
        </select>
        {cond.op !== 'always' && (
          <input
            value={cond.value}
            onChange={(e) => setCond({ value: e.target.value })}
            placeholder="value / regex"
            style={{ height: 24, padding: '0 6px', flex: 1, minWidth: 80, fontFamily: 'ui-monospace, monospace', fontSize: 11 }}
          />
        )}
      </div>
      {prog?.status === 'branch' && (
        <div style={{ fontSize: 11, color: 'var(--accent)' }}>→ took “{prog.taken}”</div>
      )}
      <Branch label="then" color="var(--chip-ok-border)">
        <NodeList nodes={node.then} onChange={(then) => onChange({ ...node, then })} tools={tools} progress={progress} />
      </Branch>
      <Branch label="else" color="var(--chip-warn-border)">
        <NodeList nodes={node.else} onChange={(els) => onChange({ ...node, else: els })} tools={tools} progress={progress} />
      </Branch>
    </div>
  )
}

function Branch({ label, color, children }: { label: string; color: string; children: React.ReactNode }): React.ReactElement {
  return (
    <div style={{ borderLeft: `2px solid ${color}`, paddingLeft: 8, marginLeft: 2, display: 'flex', flexDirection: 'column', gap: 4 }}>
      <span style={{ ...CONTROL_LABEL, textTransform: 'uppercase', letterSpacing: 0.4 }}>{label}</span>
      {children}
    </div>
  )
}

function ProgressLine({ prog }: { prog: PipeProgress }): React.ReactElement {
  const color = prog.status === 'error' ? '#e06c6c' : prog.status === 'running' ? 'var(--accent)' : 'var(--chip-ok-text)'
  return (
    <div style={{ fontSize: 11, color }}>
      {prog.status === 'running' && '● running…'}
      {prog.status === 'done' && `✓ ${(prog.output ?? '').slice(0, 200) || 'done'}`}
      {prog.status === 'error' && `✕ ${prog.error ?? 'failed'}`}
    </div>
  )
}

export function PipelineEditor({ workflow, onChange }: WorkflowEditorProps): React.ReactElement {
  const data = workflow.data as PipelineData
  const [tools, setTools] = useState<ToolInfo[]>([])
  const [running, setRunning] = useState(false)
  const [progress, setProgress] = useState<ProgressMap>({})
  const [runError, setRunError] = useState<string | null>(null)
  const mounted = useRef(true)

  useEffect(() => {
    mounted.current = true
    void window.rev.workflows.listTools().then((t) => {
      if (mounted.current) setTools(t)
    })
    return () => {
      mounted.current = false
    }
  }, [])

  const setData = (patch: Partial<PipelineData>): void => onChange({ ...workflow, data: { ...data, ...patch } })

  const run = async (): Promise<void> => {
    const resolved = resolvePipeline(data.nodes, data.vars)
    if (!resolved.ok) {
      setRunError(resolved.error)
      return
    }
    if (resolved.nodes.length === 0) {
      setRunError('Pipeline is empty')
      return
    }
    setRunError(null)
    setProgress({})
    setRunning(true)
    try {
      await window.rev.workflows.runPipeline(resolved.nodes, (p) => {
        setProgress((prev) => ({ ...prev, [p.nodeId]: p }))
      })
    } catch (e) {
      setRunError(e instanceof Error ? e.message : String(e))
    } finally {
      if (mounted.current) setRunning(false)
    }
  }

  const cancel = (): void => {
    void window.rev.workflows.cancel()
  }

  const rootProgress = useMemo(() => progress, [progress])

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        Name
        <input
          value={workflow.name}
          onChange={(e) => onChange({ ...workflow, name: e.target.value })}
          placeholder="Probe endpoint, branch on 200 vs 403"
          style={{ height: 28, padding: '0 8px' }}
        />
      </label>

      {tools.length === 0 ? (
        <div style={{ color: 'var(--text-dim)', fontStyle: 'italic' }}>Loading tools…</div>
      ) : (
        <NodeList
          nodes={data.nodes}
          onChange={(nodes) => setData({ nodes })}
          tools={tools}
          progress={rootProgress}
        />
      )}

      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        Variables (JSON, optional) — reference as {'{{name}}'} in tool inputs
        <textarea
          value={data.vars}
          onChange={(e) => setData({ vars: e.target.value })}
          spellCheck={false}
          rows={2}
          placeholder='{ "host": "example.com" }'
          style={MONO_INPUT}
        />
      </label>

      {runError && <div style={{ color: '#e06c6c', fontSize: 12 }}>{runError}</div>}

      <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
        {running ? (
          <button type="button" onClick={cancel} style={{ background: 'var(--surface-3)' }}>Stop</button>
        ) : (
          <button
            type="button"
            onClick={run}
            disabled={data.nodes.length === 0}
            style={{ background: 'var(--accent)', color: '#fff', borderColor: 'var(--accent)' }}
          >
            ▶ Run pipeline
          </button>
        )}
        <span style={{ ...CONTROL_LABEL, alignSelf: 'center' }}>
          Runs on the active tab. Branches evaluate the previous tool's result.
        </span>
      </div>
    </div>
  )
}
