/** Code-style preview for `.webclaw` recipes (`lang=log`; no engine extension table). */
import type { ReactNode } from 'react'
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots'
import { CodeBlock } from '@deepseek-ai/dsh-client-ui-primitives'
import type { DocumentPreviewProps } from '@deepseek-ai/dsh-client-ui-sidebar-documentpreview/client'
import { extractRecipeParams } from '../params.ts'
import type {} from './locales.ts'

/** Document owner props plus this preview's copy. */
export type WebclawPreviewProps = DocumentPreviewProps & PropsLocale<'webclawPreview'>

/**
 * Highlight `.webclaw` as log/plain recipe text.
 * Shows declared「参数」defaults above the body (read-only; edit via chat form on run).
 * @param props - document content from the sidebar owner.
 */
export function WebclawPreview({
  content,
  wrap,
  scrollportRef,
  t,
}: WebclawPreviewProps): ReactNode {
  if (content.kind !== 'text') return null
  const params = extractRecipeParams(content.text)
  return (
    <div data-webclaw-preview="" data-wrap={wrap ? 'true' : 'false'}>
      {params.length > 0 ? (
        <div
          data-webclaw-params=""
          style={{
            margin: '0 0 12px',
            padding: '10px 12px',
            borderBottom: '1px solid color-mix(in srgb, currentColor 12%, transparent)',
            fontSize: 13,
            lineHeight: 1.5,
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: 6 }}>{t('paramsTitle')}</div>
          <dl style={{ margin: 0, display: 'grid', gap: 4 }}>
            {params.map(p => (
              <div
                key={p.name}
                style={{ display: 'grid', gridTemplateColumns: '7em 1fr', gap: 8 }}
              >
                <dt style={{ opacity: 0.7, margin: 0 }}>{p.name}</dt>
                <dd style={{ margin: 0, fontFamily: 'ui-monospace, monospace' }}>
                  {p.value}
                </dd>
              </div>
            ))}
          </dl>
          <div style={{ marginTop: 8, opacity: 0.65, fontSize: 12 }}>
            {t('paramsHint')}
          </div>
        </div>
      ) : null}
      <CodeBlock
        contentRef={scrollportRef}
        code={content.text}
        lang="log"
        streaming={!content.eof}
        wrap={wrap}
        lineNumbers
        copyLabel={t('copy')}
        copiedLabel={t('copied')}
        toolbarLabels={{
          codeLabel: t('codeBlockTitle'),
          wrapLabel: t('wrap'),
          unwrapLabel: t('unwrap'),
        }}
      />
    </div>
  )
}
