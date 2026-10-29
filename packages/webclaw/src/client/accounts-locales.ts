/** Locale for Settings → 网站账号. */
import type {} from '@deepseek-ai/dsh-client-ui-slots'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface LocaleNamespaceMap {
    webclawAccounts: keyof typeof zh
  }
}

export const zh = {
  sectionLabel: '网站账号',
  intro:
    '登录地址与可选账号密码只保存在本机 webclaw 保险箱（Windows 下 DPAPI，macOS/Linux 为本机混淆存储）。飞书 SSO 等只需填写正确的登录 URL（如 …/login?sso=lark&…），密码可留空。密码框留空表示不修改已保存的密码；勿把密码发到对话里。',
  loading: '正在连接本机配置服务…',
  loadFailed: '加载失败',
  url: '登录地址',
  username: '账号',
  password: '密码',
  passwordPlaceholder: '留空 = 保持原密码',
  save: '保存',
  saving: '保存中…',
  saved: '已保存到本机',
  saveFailed: '保存失败',
  hasPassword: '已设密码',
  noPassword: '未设密码',
  openStandalone: '在浏览器中打开本机配置页',
  offlineTitle: '本机账号配置服务未就绪',
  offlineHint:
    '请确认 DSH 已加载 webclaw 插件且 Host 在本机运行。也可运行本机启动脚本（无需把 webclaw-lite 加入 PATH）：',
  offlineCmd:
    'Windows: %LOCALAPPDATA%\\webclaw\\bin\\webclaw-accounts.cmd\n' +
    'macOS: ~/.webclaw/bin/webclaw-accounts\n' +
    '（首次启动 DSH / 调用 webclaw 工具后会自动生成）',
}

export const en = {
  sectionLabel: 'Site accounts',
  intro:
    'Login URL and optional credentials stay in the local webclaw vault (DPAPI on Windows; obfuscated storage on macOS/Linux). Feishu SSO only needs the correct login URL (e.g. …/login?sso=lark&…); password may be blank. Leave password blank to keep the stored secret — never paste passwords into chat.',
  loading: 'Connecting to local accounts service…',
  loadFailed: 'Failed to load',
  url: 'Login URL',
  username: 'Username',
  password: 'Password',
  passwordPlaceholder: 'Blank = keep existing',
  save: 'Save',
  saving: 'Saving…',
  saved: 'Saved locally',
  saveFailed: 'Save failed',
  hasPassword: 'Password set',
  noPassword: 'No password',
  openStandalone: 'Open standalone page in browser',
  offlineTitle: 'Local accounts service is offline',
  offlineHint:
    'Make sure the webclaw plugin is loaded and the Host runs on this machine. Or open the launcher (no PATH needed):',
  offlineCmd:
    'Windows: %LOCALAPPDATA%\\webclaw\\bin\\webclaw-accounts.cmd\n' +
    'macOS: ~/.webclaw/bin/webclaw-accounts\n' +
    '(created after DSH boot / first webclaw tool call)',
} satisfies Record<keyof typeof zh, string>

export const ACCOUNTS_NS = 'webclawAccounts'
