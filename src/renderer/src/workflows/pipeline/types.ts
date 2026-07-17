// Pipeline node graph (editor form). Tool inputs are JSON text (may contain
// {{var}}); resolve.ts compiles this into the runnable tree the executor takes.
export interface PipeCond {
  on: 'output' | 'error'
  op: 'contains' | 'equals' | 'matches' | 'always'
  value: string
}

export type PipeNode =
  | { id: string; type: 'tool'; tool: string; input: string }
  | { id: string; type: 'if'; cond: PipeCond; then: PipeNode[]; else: PipeNode[] }

export interface PipelineData {
  nodes: PipeNode[]
  vars: string
}

export function nodeId(): string {
  return `n${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`
}
