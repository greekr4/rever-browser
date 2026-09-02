import { useEffect, useState } from 'react'

import { useT } from '@/stores/i18n'

interface CopiedBox {
  left: number
  top: number
  width: number
  height: number
}

interface Copied {
  box: CopiedBox | null
  key: number
}

// The active (visible) webview's on-screen rect, so we can map the picked
// element's in-page coordinates to window coordinates.
function activeWebviewRect(): DOMRect | null {
  const wvs = Array.from(document.querySelectorAll('webview')) as HTMLElement[]
  for (const w of wvs) {
    if (w.offsetParent === null) continue
    const r = w.getBoundingClientRect()
    if (r.width > 0 && r.height > 0) return r
  }
  return null
}

// "Copy Element" confirmation: outline the picked element with a "Copied!"
// badge at its real position, plus a brief bottom toast confirming the
// clipboard write. Fires from the main-process copy paths via IPC; self-clears
// when the (longest) toast animation ends.
export function CopyToast(): React.ReactElement | null {
  const t = useT()
  const [copied, setCopied] = useState<Copied | null>(null)

  useEffect(() => {
    return window.rev.onElementCopied((p) => {
      let box: CopiedBox | null = null
      if (p.rect) {
        const wr = activeWebviewRect()
        if (wr) {
          box = {
            left: wr.left + p.rect.x,
            top: wr.top + p.rect.y,
            width: p.rect.width,
            height: p.rect.height
          }
        }
      }
      setCopied({ box, key: Date.now() })
    })
  }, [])

  if (!copied) return null

  return (
    <>
      {copied.box && (
        <div
          key={`box-${copied.key}`}
          className="copy-box"
          style={{
            left: copied.box.left,
            top: copied.box.top,
            width: copied.box.width,
            height: copied.box.height
          }}
          aria-hidden
        >
          <span className="copy-box-label">{t('copy.copied')}</span>
        </div>
      )}
      <div
        key={`toast-${copied.key}`}
        className="copy-clip-toast"
        role="status"
        onAnimationEnd={(e) => {
          if (e.animationName === 'copy-clip-toast') setCopied(null)
        }}
      >
        {t('copy.toClipboard')}
      </div>
    </>
  )
}
