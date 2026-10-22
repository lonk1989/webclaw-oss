/** Code-style preview for `.webclaw` recipes (`lang=log`; no engine extension table). */
import type { ReactNode } from 'react'
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots'
import { CodeBlock } from '@deepseek-ai/dsh-client-ui-primitives'
import type { DocumentPreviewProps } from '@deepseek-ai/dsh-client-ui-sidebar-documentpreview/client'
import type {} from './locales.ts'

/** Document owner props plus this preview's copy. */
export type WebclawPreviewProps = DocumentPreviewProps & PropsLocale<'webclawPreview'>

/**
 * Highlight `.webclaw` as log/plain recipe text.
 * @param props - document content from the sidebar owner.
 */
export function WebclawPreview({
  content,
  wrap,
  scrollportRef,
  t,
}: WebclawPreviewProps): ReactNode {
  if (content.kind !== 'text') return null
  return (
    <div data-webclaw-preview="" data-wrap={wrap ? 'true' : 'false'}>
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
