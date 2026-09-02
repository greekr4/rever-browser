import { useCallback, useEffect, useRef, useState, type MutableRefObject } from 'react'

import type { WebviewTabHandle } from '@/components/WebviewTab'
import { useTabsStore } from '@/stores/tabs'

export interface FindInPage {
  findOpen: boolean
  setFindOpen: (open: boolean) => void
  findQuery: string
  setFindQuery: (q: string) => void
  findResult: { active: number; total: number } | null
  findInputRef: React.RefObject<HTMLInputElement | null>
  findTabRef: MutableRefObject<string | null>
  doFind: (query: string, opts?: { forward?: boolean; findNext?: boolean }) => void
  closeFind: () => void
}

// Cmd/Ctrl+F find bar. Acts on the active tab's webview and closes when the user
// switches tabs. Extracted from App; it needs the shared tab-handle map and the
// active tab id.
export function useFindInPage(
  tabRefs: MutableRefObject<Map<string, WebviewTabHandle>>,
  activeId: string | null
): FindInPage {
  const [findOpen, setFindOpen] = useState(false)
  const [findQuery, setFindQuery] = useState('')
  const [findResult, setFindResult] = useState<{ active: number; total: number } | null>(null)
  const findInputRef = useRef<HTMLInputElement>(null)
  const findTabRef = useRef<string | null>(null)

  const doFind = useCallback(
    (query: string, opts?: { forward?: boolean; findNext?: boolean }) => {
      const id = useTabsStore.getState().activeId
      const handle = tabRefs.current.get(id)
      if (!handle) return
      if (!query) {
        handle.stopFindInPage('clearSelection')
        setFindResult(null)
        return
      }
      handle.findInPage(query, opts)
    },
    [tabRefs]
  )

  const closeFind = useCallback(() => {
    const id = findTabRef.current ?? useTabsStore.getState().activeId
    tabRefs.current.get(id)?.stopFindInPage('clearSelection')
    findTabRef.current = null
    setFindOpen(false)
    setFindResult(null)
  }, [tabRefs])

  // Live match count from the active tab while the bar is open.
  useEffect(() => {
    if (!findOpen) return
    const handle = activeId ? tabRefs.current.get(activeId) : undefined
    return handle?.onFoundInPage((r) =>
      setFindResult({ active: r.activeMatchOrdinal, total: r.matches })
    )
  }, [findOpen, activeId, tabRefs])

  // Switching tabs closes the bar (and clears highlights on the old tab).
  useEffect(() => {
    if (findOpen && findTabRef.current && activeId !== findTabRef.current) closeFind()
  }, [activeId, findOpen, closeFind])

  return {
    findOpen,
    setFindOpen,
    findQuery,
    setFindQuery,
    findResult,
    findInputRef,
    findTabRef,
    doFind,
    closeFind
  }
}
