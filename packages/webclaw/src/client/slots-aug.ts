/**
 * Soft slot contract for Settings when `@deepseek-ai/dsh-client-ui-settings`
 * is present at runtime (optional peer).
 */
import type { ReactNode } from 'react'
import type {} from '@deepseek-ai/dsh-client-ui-slots'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface SlotMap {
    'settings.section': {
      kind: 'list'
      scope: 'root'
      owner: {
        id: string
        order?: number
        label?: string | (() => string)
        children?: ReactNode
      }
    }
  }
}
