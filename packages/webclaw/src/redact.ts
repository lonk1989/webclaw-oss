/**
 * Scrub secrets from tool output before it reaches the model / chat.
 */

const PASSWORD_STEP =
  /(?:输入\s*)?密码\s*=\s*[^，,\n\r"']+/gi
const JSON_PASSWORD = /("(?:password|pass|pwd|密码)"\s*:\s*")([^"]*)(")/gi
const PLACEHOLDER_PASSWORD = /\{\{\s*[^}]*密码[^}]*\s*\}\}/gi
const CLI_PASSWORD_FLAG = /(--password|--pass)\s+\S+/gi

/** Best-effort redaction for eventsTail / stderr echoed to DSH. */
export function redactSecrets(text: string): string {
  if (!text) return text
  return text
    .replace(PASSWORD_STEP, '密码=***')
    .replace(JSON_PASSWORD, '$1***$3')
    .replace(PLACEHOLDER_PASSWORD, '{{***.密码}}')
    .replace(CLI_PASSWORD_FLAG, '$1 ***')
}
