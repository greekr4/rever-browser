import { useCallback, useEffect, useRef, useState } from 'react'

import { useT } from '@/stores/i18n'

// Screenshot annotation overlay. Draws the captured image on a canvas and lets
// the user mark it up with rectangles, arrows, and freehand strokes, then copy
// the result to the clipboard or save it. Fed by Grab (element capture).

type Tool = 'rect' | 'arrow' | 'pen'

interface Point {
  x: number
  y: number
}

type Shape =
  | { type: 'rect'; color: string; x: number; y: number; w: number; h: number }
  | { type: 'arrow'; color: string; x1: number; y1: number; x2: number; y2: number }
  | { type: 'pen'; color: string; points: Point[] }

const COLORS = ['#ef4444', '#f59e0b', '#22c55e', '#3b82f6', '#ffffff', '#111111']

interface Props {
  imageDataUrl: string
  onClose: () => void
}

export function MarkupEditor({ imageDataUrl, onClose }: Props) {
  const t = useT()
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const imgRef = useRef<HTMLImageElement | null>(null)
  const [tool, setTool] = useState<Tool>('rect')
  const [color, setColor] = useState<string>(COLORS[0])
  const [shapes, setShapes] = useState<Shape[]>([])
  const [ready, setReady] = useState(false)
  const [copied, setCopied] = useState(false)

  const drafting = useRef<Shape | null>(null)
  const lineWidth = useRef(3)

  // Load the image once; size the canvas to its natural resolution.
  useEffect(() => {
    const img = new Image()
    img.onload = () => {
      imgRef.current = img
      const canvas = canvasRef.current
      if (canvas) {
        canvas.width = img.naturalWidth
        canvas.height = img.naturalHeight
      }
      lineWidth.current = Math.max(2, Math.round(img.naturalWidth / 320))
      setReady(true)
    }
    img.src = imageDataUrl
  }, [imageDataUrl])

  // Redraw the image + every committed shape (+ the in-progress one).
  const redraw = useCallback(() => {
    const canvas = canvasRef.current
    const img = imgRef.current
    if (!canvas || !img) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.clearRect(0, 0, canvas.width, canvas.height)
    ctx.drawImage(img, 0, 0)
    const all = drafting.current ? [...shapes, drafting.current] : shapes
    for (const s of all) drawShape(ctx, s, lineWidth.current)
  }, [shapes])

  useEffect(() => {
    if (ready) redraw()
  }, [ready, shapes, redraw])

  const toCanvasPoint = (e: React.PointerEvent): Point => {
    const canvas = canvasRef.current!
    const rect = canvas.getBoundingClientRect()
    return {
      x: ((e.clientX - rect.left) / rect.width) * canvas.width,
      y: ((e.clientY - rect.top) / rect.height) * canvas.height
    }
  }

  const onPointerDown = (e: React.PointerEvent): void => {
    ;(e.target as Element).setPointerCapture(e.pointerId)
    const p = toCanvasPoint(e)
    if (tool === 'rect') drafting.current = { type: 'rect', color, x: p.x, y: p.y, w: 0, h: 0 }
    else if (tool === 'arrow')
      drafting.current = { type: 'arrow', color, x1: p.x, y1: p.y, x2: p.x, y2: p.y }
    else drafting.current = { type: 'pen', color, points: [p] }
    redraw()
  }

  const onPointerMove = (e: React.PointerEvent): void => {
    const d = drafting.current
    if (!d) return
    const p = toCanvasPoint(e)
    if (d.type === 'rect') {
      d.w = p.x - d.x
      d.h = p.y - d.y
    } else if (d.type === 'arrow') {
      d.x2 = p.x
      d.y2 = p.y
    } else {
      d.points.push(p)
    }
    redraw()
  }

  const onPointerUp = (): void => {
    const d = drafting.current
    drafting.current = null
    if (!d) return
    // Discard zero-size clicks (no drag).
    if (d.type === 'rect' && Math.abs(d.w) < 3 && Math.abs(d.h) < 3) return redraw()
    if (d.type === 'pen' && d.points.length < 2) return redraw()
    setShapes((s) => [...s, d])
  }

  const copy = async (): Promise<void> => {
    const canvas = canvasRef.current
    if (!canvas) return
    canvas.toBlob(async (blob) => {
      if (!blob) return
      try {
        await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })])
        setCopied(true)
        setTimeout(() => setCopied(false), 1500)
      } catch (err) {
        console.error('[markup] clipboard write failed', err)
      }
    }, 'image/png')
  }

  const download = (): void => {
    const canvas = canvasRef.current
    if (!canvas) return
    const a = document.createElement('a')
    a.href = canvas.toDataURL('image/png')
    a.download = `rever-markup-${Date.now()}.png`
    a.click()
  }

  return (
    <div
      onClick={(e) => e.target === e.currentTarget && onClose()}
      style={{
        position: 'fixed',
        inset: 0,
        background: 'rgba(0,0,0,0.6)',
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 10,
        zIndex: 2000,
        padding: 20
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          padding: '8px 12px',
          background: 'var(--bg-bar)',
          border: '1px solid var(--border-2)',
          borderRadius: 8,
          flexWrap: 'wrap'
        }}
      >
        <ToolBtn label="▭" active={tool === 'rect'} onClick={() => setTool('rect')} title={t('markup.rect')} />
        <ToolBtn label="↗" active={tool === 'arrow'} onClick={() => setTool('arrow')} title={t('markup.arrow')} />
        <ToolBtn label="✎" active={tool === 'pen'} onClick={() => setTool('pen')} title={t('markup.pen')} />
        <div style={{ width: 1, height: 20, background: 'var(--border-2)', margin: '0 4px' }} />
        {COLORS.map((c) => (
          <button
            key={c}
            type="button"
            onClick={() => setColor(c)}
            title={c}
            style={{
              width: 18,
              height: 18,
              borderRadius: '50%',
              background: c,
              border: color === c ? '2px solid var(--accent)' : '1px solid var(--border-2)',
              cursor: 'pointer',
              padding: 0
            }}
          />
        ))}
        <div style={{ width: 1, height: 20, background: 'var(--border-2)', margin: '0 4px' }} />
        <BarBtn onClick={() => setShapes((s) => s.slice(0, -1))} disabled={shapes.length === 0}>
          {t('markup.undo')}
        </BarBtn>
        <BarBtn onClick={() => setShapes([])} disabled={shapes.length === 0}>
          {t('markup.clear')}
        </BarBtn>
        <BarBtn onClick={() => void copy()} primary>
          {copied ? t('markup.copied') : t('markup.copy')}
        </BarBtn>
        <BarBtn onClick={download}>{t('markup.save')}</BarBtn>
        <BarBtn onClick={onClose}>{t('markup.close')}</BarBtn>
      </div>

      <div style={{ maxWidth: '90vw', maxHeight: '78vh', overflow: 'auto', borderRadius: 6 }}>
        <canvas
          ref={canvasRef}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          style={{
            display: 'block',
            maxWidth: '90vw',
            maxHeight: '78vh',
            cursor: 'crosshair',
            touchAction: 'none',
            background: '#fff'
          }}
        />
      </div>
    </div>
  )
}

function drawShape(ctx: CanvasRenderingContext2D, s: Shape, lw: number): void {
  ctx.strokeStyle = s.color
  ctx.fillStyle = s.color
  ctx.lineWidth = lw
  ctx.lineCap = 'round'
  ctx.lineJoin = 'round'
  if (s.type === 'rect') {
    ctx.strokeRect(s.x, s.y, s.w, s.h)
  } else if (s.type === 'arrow') {
    ctx.beginPath()
    ctx.moveTo(s.x1, s.y1)
    ctx.lineTo(s.x2, s.y2)
    ctx.stroke()
    // Arrowhead.
    const angle = Math.atan2(s.y2 - s.y1, s.x2 - s.x1)
    const head = lw * 4
    ctx.beginPath()
    ctx.moveTo(s.x2, s.y2)
    ctx.lineTo(s.x2 - head * Math.cos(angle - Math.PI / 6), s.y2 - head * Math.sin(angle - Math.PI / 6))
    ctx.moveTo(s.x2, s.y2)
    ctx.lineTo(s.x2 - head * Math.cos(angle + Math.PI / 6), s.y2 - head * Math.sin(angle + Math.PI / 6))
    ctx.stroke()
  } else {
    ctx.beginPath()
    s.points.forEach((p, i) => (i === 0 ? ctx.moveTo(p.x, p.y) : ctx.lineTo(p.x, p.y)))
    ctx.stroke()
  }
}

function ToolBtn({
  label,
  active,
  onClick,
  title
}: {
  label: string
  active: boolean
  onClick: () => void
  title: string
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      style={{
        width: 30,
        height: 26,
        fontSize: 15,
        lineHeight: 1,
        background: active ? 'var(--accent-soft)' : 'transparent',
        border: `1px solid ${active ? 'var(--accent-border)' : 'var(--border-2)'}`,
        borderRadius: 4,
        color: active ? 'var(--accent)' : 'var(--text-2)',
        cursor: 'pointer'
      }}
    >
      {label}
    </button>
  )
}

function BarBtn({
  children,
  onClick,
  disabled,
  primary
}: {
  children: React.ReactNode
  onClick: () => void
  disabled?: boolean
  primary?: boolean
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      style={{
        padding: '5px 10px',
        fontSize: 11,
        background: primary ? 'var(--accent)' : 'transparent',
        border: `1px solid ${primary ? 'var(--accent)' : 'var(--border-2)'}`,
        borderRadius: 4,
        color: primary ? '#fff' : 'var(--text-2)',
        cursor: disabled ? 'default' : 'pointer',
        opacity: disabled ? 0.4 : 1
      }}
    >
      {children}
    </button>
  )
}
