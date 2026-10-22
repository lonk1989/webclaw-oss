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
  paramsTitle: '可改参数（默认值）',
  paramsHint: '菜谱参数是占位/默认值；有 参数…= 时先确认或「用默认」。业务指令（如请假）用户已给齐字段则直接跑，勿反复追问。',
}

/** English dictionary with the same keys. */
export const en = {
  title: 'Recipe',
  copy: 'Copy',
  copied: 'Copied',
  codeBlockTitle: 'Recipe',
  wrap: 'Wrap',
  unwrap: 'Unwrap',
  paramsTitle: 'Editable params (defaults)',
  paramsHint: 'On run, the agent will show a param form in chat before starting.',
} satisfies Record<keyof typeof zh, string>

export const NS = 'webclawPreview'
