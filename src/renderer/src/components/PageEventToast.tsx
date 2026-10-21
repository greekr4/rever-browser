import { useEffect, useState } from 'react'

import type { PageEvent } from '../../../preload'
import { useT } from '@/stores/i18n'

const SHOW_MS = 6000
const MAX_VISIBLE = 3

interface Item {
  key: number
  event: PageEvent
}

let seq = 0

// Bottom-right notices for things a page did off-screen: a finished download
// (with a way to find the file) and a dialog the app answered for the agent.
export function PageEventToast(): React.ReactElement | null {
  const t = useT()
  const [items, setItems] = useState<Item[]>([])

  useEffect(
    () =>
      window.rev.onPageEvent((event) => {
        const key = ++seq
        setItems((prev) => [...prev.slice(-(MAX_VISIBLE - 1)), { key, event }])
        setTimeout(() => setItems((prev) => prev.filter((i) => i.key !== key)), SHOW_MS)
      }),
    []
  )

  if (items.length === 0) return null

  return (
    <div className="page-event-stack" role="status">
      {items.map(({ key, event }) => (
        <div key={key} className="page-event-toast">
          {event.kind === 'download' ? (
            event.download.state === 'completed' ? (
              <>
                <span>{t('toast.downloaded', { name: event.download.filename })}</span>
                <button onClick={() => void window.rev.downloads.reveal(event.download.id)}>
                  {t('toast.showInFolder')}
                </button>
              </>
            ) : (
              <span>
                {t('toast.downloadFailed', {
                  name: event.download.filename,
                  state: event.download.state
                })}
              </span>
            )
          ) : (
            <span>
              {t('toast.dialogAnswered', {
                type: event.type,
                answer: event.accept ? 'OK' : 'Cancel',
                message: event.message.length > 80 ? `${event.message.slice(0, 80)}…` : event.message
              })}
            </span>
          )}
        </div>
      ))}
    </div>
  )
}
