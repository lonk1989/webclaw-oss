/**
 * Client surfaces for `.webclaw`: file-tree icon tint + sidebar recipe preview.
 */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar-documentpreview/client'
import type {} from '@deepseek-ai/dsh-client-ui-slots'
import { WEBCLAW_ICON_CSS } from './icons-css.ts'
import { en, NS, zh } from './locales.ts'
import { WebclawPreview } from './WebclawPreview.tsx'

/** Document-preview implementation id (slot key). */
export const WEBCLAW_PREVIEW_ID = '@lonk1989/webclaw/recipe'

/**
 * Register preview + global icon CSS on the client fiber.
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
    const t = scope.locale.bind(NS)
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
  })
}
