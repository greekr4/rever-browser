/// <reference types="vite/client" />

import type { RevAPI } from '../../preload'

declare global {
  interface Window {
    rev: RevAPI
  }
}

export {}
