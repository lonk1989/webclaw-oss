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
  paramsTitle: '可改参数（多为占位，请编辑）',
  paramsHint: '有 参数…= 时先展示一张可编辑表请用户改完再跑；勿逐项追问。仅用户明确说「用默认」才可不改。本轮已给齐字段则可直接跑。',
}

/** English dictionary with the same keys. */
export const en = {
  title: 'Recipe',
  copy: 'Copy',
  copied: 'Copied',
  codeBlockTitle: 'Recipe',
  wrap: 'Wrap',
  unwrap: 'Unwrap',
  paramsTitle: 'Editable params (usually placeholders)',
  paramsHint: 'On run, show one editable table for the user to change values. Do not interview field-by-field;「use defaults」only if asked.',
} satisfies Record<keyof typeof zh, string>

export const NS = 'webclawPreview'
