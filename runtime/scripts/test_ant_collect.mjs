import fs from 'fs';
import { JSDOM } from 'jsdom';

const fixture = process.argv[2] || 'data/html/table_ant_split_min.html';
const expectName = process.argv[3] || '吉庆';

const rs = fs.readFileSync('src/page_collect.rs', 'utf8');
const m = rs.match(/const PAGE_COLLECT_JS: &str = r#"([\s\S]*?)"#;/);
if (!m) {
  console.error('PAGE_COLLECT_JS not found');
  process.exit(1);
}
const body = m[1];
let html = fs.readFileSync(fixture, 'utf8');
if (!/<html/i.test(html)) {
  html = `<!DOCTYPE html><html><head><meta charset="utf-8"></head><body>${html}</body></html>`;
}
const dom = new JSDOM(html, { runScripts: 'outside-only', pretendToBeVisual: true });
const { window } = dom;
const { document } = window;

for (const row of document.querySelectorAll('tr')) {
  let x = 0;
  for (const el of row.children) {
    const w = 60;
    const left = x;
    el.getBoundingClientRect = () => ({
      x: left,
      y: 0,
      left,
      top: 0,
      width: w,
      height: 24,
      right: left + w,
      bottom: 24,
      toJSON() {},
    });
    x += w;
  }
}
for (const el of document.querySelectorAll('table,div')) {
  const r = {
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    width: 800,
    height: 400,
    right: 800,
    bottom: 400,
    toJSON() {},
  };
  el.getBoundingClientRect = () => r;
}
window.getComputedStyle = () => ({
  display: 'block',
  visibility: 'visible',
  opacity: '1',
  position: 'static',
  top: 'auto',
  left: 'auto',
  height: '24px',
  width: '60px',
  overflow: 'auto',
  overflowX: 'auto',
});

const result = await window.eval(
  `(async () => { const WANT_TABLES = true; ${body} })()`,
);
const t = (result.tables || [])[0];
const summary = {
  fixture,
  ok: result.ok,
  tableCount: (result.tables || []).length,
  cols: t?.headers?.length,
  rows: t?.rows?.length,
  headersSample: t?.headers?.slice(0, 8),
  row0Sample: t?.rows?.[0]?.slice(0, 8),
};
console.log(JSON.stringify(summary, null, 2));
if (!t) process.exit(2);
if (!t.headers.includes('员工姓名') || !t.rows.some((r) => r.includes(expectName))) {
  console.error('missing expected cells');
  process.exit(3);
}
console.log('PASS');
