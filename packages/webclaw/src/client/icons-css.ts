/** File-tree icon tint for `.webclaw` (matches engine `code` color without patching FileTypeIcon). */
export const WEBCLAW_ICON_CSS = `
[data-files-entry='file'][data-files-path$='.webclaw' i] svg,
[data-files-entry='file'][data-files-path$='.WEBCLAW'] svg {
  color: var(--dsw-static-deepseek-500) !important;
  --dsh-file-type-icon-color: var(--dsw-static-deepseek-500);
  --dsh-file-type-default-color: var(--dsw-static-deepseek-500);
}
[data-webclaw-preview] {
  display: flex;
  flex: 1;
  min-height: 0;
  flex-direction: column;
}
[data-webclaw-preview] > * {
  flex: 1;
  min-height: 0;
}
`
