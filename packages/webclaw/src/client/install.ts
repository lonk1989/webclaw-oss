/**
 * Client surfaces: `.webclaw` preview + Settings → 网站账号.
 */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar-documentpreview/client'
import type {} from '@deepseek-ai/dsh-client-ui-slots'
import type {} from './slots-aug.ts'
import { AccountsSettings } from './AccountsSettings.tsx'
import {
  ACCOUNTS_NS,
  en as accountsEn,
  zh as accountsZh,
} from './accounts-locales.ts'
import { WEBCLAW_ICON_CSS } from './icons-css.ts'
import { en, NS, zh } from './locales.ts'
import { WebclawPreview } from './WebclawPreview.tsx'

/** Document-preview implementation id (slot key). */
export const WEBCLAW_PREVIEW_ID = '@lonk1989/webclaw/recipe'

/** Settings section id for site accounts. */
export const WEBCLAW_ACCOUNTS_SECTION = 'webclaw-accounts'

/**
 * Register preview + accounts settings + file-tree icon CSS.
 * @param ctx - client root context.
 */
export function installWebclawSurfaces(ctx: Context): void {
  ctx.effect(() => {
    const style = document.createElement('style')
    style.setAttribute('data-webclaw-icons', '')
    style.textContent = WEBCLAW_ICON_CSS
    document.head.appendChild(style)
    return () => {
      style.remove()
    }
  }, 'webclaw: file icons')

  ctx.inject(['documentPreviews', 'slots', 'locale'], scope => {
    scope.effect(
      () => scope.locale.register(NS, { zh, en }),
      'webclaw: preview dictionaries',
    )
    scope.effect(
      () => scope.locale.register(ACCOUNTS_NS, { zh: accountsZh, en: accountsEn }),
      'webclaw: accounts dictionaries',
    )
    const t = scope.locale.bind(NS)
    const ta = scope.locale.bind(ACCOUNTS_NS)
    scope.effect(
      () =>
        scope.documentPreviews.register({
          id: WEBCLAW_PREVIEW_ID,
          extensions: ['webclaw'],
          priority: 'extension',
          title: () => t('title'),
          loading: 'text-pages',
          wrap: true,
        }),
      'webclaw: preview metadata',
    )
    scope.effect(
      () =>
        scope.slots.inject('sidebar.right.tab.document', () =>
          scope.slots.register(
            {
              name: 'sidebar.right.tab.document',
              key: WEBCLAW_PREVIEW_ID,
              locale: NS,
            },
            WebclawPreview,
          ),
        ),
      'webclaw: preview body',
    )
    // Soft: settings shell may be absent in minimal compositions.
    scope.effect(
      () =>
        scope.slots.inject('settings.section', () =>
          scope.slots.register(
            {
              name: 'settings.section',
              id: WEBCLAW_ACCOUNTS_SECTION,
              order: 45,
              label: () => ta('sectionLabel'),
              locale: ACCOUNTS_NS,
            },
            AccountsSettings,
          ),
        ),
      'webclaw: settings accounts',
    )
  })
}
