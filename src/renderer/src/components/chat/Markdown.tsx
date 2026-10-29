import { memo, useRef, useState } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

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
          background: 'var(--surface)',
          border: '1px solid var(--border-2)',
          color: 'var(--text-2)',
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

export const Markdown = memo(function Markdown({ text, dim }: { text: string; dim?: boolean }) {
  return (
    <div className={`md${dim ? ' md--dim' : ''}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
        {text}
      </ReactMarkdown>
    </div>
  )
})

