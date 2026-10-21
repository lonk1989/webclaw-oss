/** Locale for the `@lonk1989/webclaw` `.webclaw` document preview. */
import type {} from '@deepseek-ai/dsh-client-ui-slots'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface LocaleNamespaceMap {
    /** Webclaw recipe preview chrome. */
    webclawPreview: keyof typeof zh
  }
}

/** Simplified Chinese dictionary and key source. */
export const zh = {
  title: '菜谱',
  copy: '复制',
  copied: '已复制',
  codeBlockTitle: '菜谱',
  wrap: '换行',
  unwrap: '不换行',
}

/** English dictionary with the same keys. */
export const en = {
  title: 'Recipe',
  copy: 'Copy',
  copied: 'Copied',
  codeBlockTitle: 'Recipe',
  wrap: 'Wrap',
  unwrap: 'Unwrap',
} satisfies Record<keyof typeof zh, string>

export const NS = 'webclawPreview'
