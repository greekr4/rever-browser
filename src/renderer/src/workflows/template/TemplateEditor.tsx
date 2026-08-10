import type { WorkflowEditorProps } from '../core/registry'

import { useT } from '@/stores/i18n'
export interface TemplateData {
  body: string
}

export function TemplateEditor({ workflow, onChange }: WorkflowEditorProps): React.ReactElement {
  const tr = useT()
  const data = workflow.data as TemplateData

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        Name
        <input
          value={workflow.name}
          onChange={(e) => onChange({ ...workflow, name: e.target.value })}
          placeholder={tr('template.goalPlaceholder')}
          style={{ height: 28, padding: '0 8px' }}
        />
      </label>
      <label style={{ display: 'flex', flexDirection: 'column', gap: 3, color: 'var(--text-dim)' }}>
        Prompt
        <textarea
          value={data.body}
          onChange={(e) => onChange({ ...workflow, data: { body: e.target.value } })}
          placeholder={tr('template.bodyPlaceholder')}
          rows={8}
          style={{
            padding: 8,
            fontFamily: 'ui-monospace, monospace',
            fontSize: 12,
            resize: 'vertical',
            background: 'var(--surface)',
            color: 'var(--text)',
            border: '1px solid var(--border-2)',
            borderRadius: 4
          }}
        />
      </label>
    </div>
  )
}
