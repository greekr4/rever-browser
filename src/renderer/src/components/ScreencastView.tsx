import { useEffect, useRef } from 'react'

interface ScreencastFrameMeta {
  offsetTop: number
  pageScaleFactor: number
  deviceWidth: number
  deviceHeight: number
  scrollOffsetX: number
  scrollOffsetY: number
  timestamp?: number
}

// Tracks the latest frame metadata for coordinate mapping
const latestMeta = { current: null as ScreencastFrameMeta | null }

export function ScreencastView() {
  const canvasRef = useRef<HTMLCanvasElement>(null)

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return

    // Start screencast
    void window.rev.external.startScreencast({ quality: 80, everyNthFrame: 1, maxWidth: 1280, maxHeight: 800 })

    // Subscribe to frames
    const off = window.rev.external.onScreencastFrame(({ data, metadata, sessionId }) => {
      const meta = metadata as ScreencastFrameMeta
      latestMeta.current = meta

      const img = new Image()
      img.onload = () => {
        const ctx = canvas.getContext('2d')
        if (!ctx) return
        canvas.width = img.naturalWidth
        canvas.height = img.naturalHeight
        ctx.drawImage(img, 0, 0)
      }
      img.src = `data:image/jpeg;base64,${data}`

      // Ack is already sent by main process on screencastFrame event,
      // but we send again from renderer as a belt-and-suspenders measure
      void window.rev.external.ackFrame(sessionId)
    })

    return () => {
      off()
      void window.rev.external.stopScreencast()
    }
  }, [])

  // Convert canvas display coords → CDP page coords
  function toPageCoords(canvasX: number, canvasY: number): { x: number; y: number } {
    const canvas = canvasRef.current
    const meta = latestMeta.current
    if (!canvas || !meta) return { x: canvasX, y: canvasY }

    const scaleX = meta.deviceWidth / canvas.offsetWidth
    const scaleY = meta.deviceHeight / canvas.offsetHeight
    const psf = meta.pageScaleFactor || 1

    return {
      x: (canvasX * scaleX) / psf + meta.scrollOffsetX,
      y: (canvasY * scaleY) / psf + meta.scrollOffsetY
    }
  }

  const onMouseEvent = (type: string) => (e: React.MouseEvent<HTMLCanvasElement>) => {
    const rect = (e.currentTarget as HTMLCanvasElement).getBoundingClientRect()
    const canvasX = e.clientX - rect.left
    const canvasY = e.clientY - rect.top
    const { x, y } = toPageCoords(canvasX, canvasY)

    const modifiers =
      (e.altKey ? 1 : 0) |
      (e.ctrlKey ? 2 : 0) |
      (e.metaKey ? 4 : 0) |
      (e.shiftKey ? 8 : 0)

    const buttonMap: Record<number, string> = { 0: 'left', 1: 'middle', 2: 'right' }
    const button = buttonMap[e.button] ?? 'none'

    void window.rev.external.dispatchMouseEvent({
      type,
      x,
      y,
      modifiers,
      button,
      buttons: e.buttons,
      clickCount: type === 'mousePressed' || type === 'mouseReleased' ? 1 : 0
    })
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLCanvasElement>) => {
    e.preventDefault()
    const modifiers =
      (e.altKey ? 1 : 0) |
      (e.ctrlKey ? 2 : 0) |
      (e.metaKey ? 4 : 0) |
      (e.shiftKey ? 8 : 0)

    const base = {
      modifiers,
      key: e.key,
      code: e.code,
      windowsVirtualKeyCode: e.keyCode,
      nativeVirtualKeyCode: e.keyCode
    }

    void window.rev.external.dispatchKeyEvent({ ...base, type: 'keyDown' })

    // For printable chars, send a char event
    if (e.key.length === 1) {
      void window.rev.external.dispatchKeyEvent({ ...base, type: 'char', text: e.key })
    }
  }

  const onKeyUp = (e: React.KeyboardEvent<HTMLCanvasElement>) => {
    e.preventDefault()
    const modifiers =
      (e.altKey ? 1 : 0) |
      (e.ctrlKey ? 2 : 0) |
      (e.metaKey ? 4 : 0) |
      (e.shiftKey ? 8 : 0)

    void window.rev.external.dispatchKeyEvent({
      type: 'keyUp',
      modifiers,
      key: e.key,
      code: e.code,
      windowsVirtualKeyCode: e.keyCode,
      nativeVirtualKeyCode: e.keyCode
    })
  }

  const onWheel = (e: React.WheelEvent<HTMLCanvasElement>) => {
    const rect = (e.currentTarget as HTMLCanvasElement).getBoundingClientRect()
    const canvasX = e.clientX - rect.left
    const canvasY = e.clientY - rect.top
    const { x, y } = toPageCoords(canvasX, canvasY)

    void window.rev.external.dispatchMouseEvent({
      type: 'mouseWheel',
      x,
      y,
      deltaX: e.deltaX,
      deltaY: e.deltaY,
      modifiers: 0
    })
  }

  return (
    <div
      style={{
        width: '100%',
        height: '100%',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        background: 'var(--viewport-bg)',
        overflow: 'hidden'
      }}
    >
      <canvas
        ref={canvasRef}
        tabIndex={0}
        style={{
          // Letterbox: keep aspect ratio of the JPEG frames (set in onload).
          // The img.naturalWidth/Height drives the canvas internal size, and
          // here we fit it to the parent while preserving aspect.
          maxWidth: '100%',
          maxHeight: '100%',
          width: 'auto',
          height: 'auto',
          objectFit: 'contain',
          display: 'block',
          cursor: 'default',
          outline: 'none'
        }}
        onMouseMove={onMouseEvent('mouseMoved')}
        onMouseDown={onMouseEvent('mousePressed')}
        onMouseUp={onMouseEvent('mouseReleased')}
        onKeyDown={onKeyDown}
        onKeyUp={onKeyUp}
        onWheel={onWheel}
      />
    </div>
  )
}
