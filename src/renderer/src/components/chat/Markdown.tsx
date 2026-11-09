import { memo, useRef, useState } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { useT } from '@/stores/i18n'

function CodeBlock({ children }: { children?: React.ReactNode }) {
  const tr = useT()
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
    <div className="code-block">
      <button type="button" className="code-copy" onClick={onCopy}>
        {copied ? tr('md.copied') : tr('md.copy')}
      </button>
      <pre ref={ref}>{children}</pre>
    </div>
  )
}

const MD_COMPONENTS = {
  pre: ({ children }: { children?: React.ReactNode }) => <CodeBlock>{children}</CodeBlock>,
  // Wide tables scroll inside their own box instead of running past the panel edge.
  table: ({ children }: { children?: React.ReactNode }) => (
    <div className="md-table">
      <table>{children}</table>
    </div>
  )
}

export const Markdown = memo(function Markdown({ text, dim }: { text: string; dim?: boolean }) {
  return (
    <div className={`md${dim ? ' md--dim' : ''}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
        {text}
      </ReactMarkdown>
    </div>
  )
})

