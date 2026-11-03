// 编辑器「斜杠命令」+ 全屏页「章节目录」浏览器验证（playwright-core）。
//
// 用法：node scripts/editor-slash-toc-e2e.mjs
// 需要先起一个 smoke server（默认 http://127.0.0.1:3099），见
// memory/browser-verification-setup.md。环境变量：RODEO_BASE / RODEO_SITE_ROOT。
//
// 覆盖 spec/20261103-补充设计&缺陷.md 的第 4、5、6 条：
//   4 → 全屏页左侧章节目录：条目数 / 层级缩进 / 点击滚动 / 滚动高亮
//   5 → 编辑器里输入 `/` 弹出控件表：过滤、回车执行、Esc 关闭
//   6 → 顶栏「新建」hover 分裂菜单：鼠标穿过按钮与菜单之间的缝隙不消失
//
// 踩过的坑（与 editor-smoke.mjs 相同）：playwright-core 是 CommonJS，按绝对路径
// default import；debug wasm 走磁盘 fulfill；localStorage 里的 jwt 是 JSON 字符串；
// 不要预置 rodeo_theme（SSR 与客户端会不一致 → hydration panic）。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.RODEO_BASE || 'http://127.0.0.1:3099';
const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const SITE_ROOT = process.env.RODEO_SITE_ROOT || 'target/site';
const EXECUTABLE = path.join(
  process.env.HOME,
  'Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64',
  'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
);

let failures = [];
function check(name, ok, detail) {
  if (ok) console.log(`  ok   ${name}`);
  else {
    console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`);
    failures.push(name);
  }
}

function publishedWasm() {
  const hashFile = path.join(REPO, 'target/debug/hash.txt');
  const hash = fs.readFileSync(hashFile, 'utf8').split('\n')
    .find((l) => l.startsWith('wasm:'))?.split(/[:\s]+/)[1];
  const p = path.join(REPO, SITE_ROOT, 'pkg', `rodeo.${hash}.wasm`);
  if (!fs.existsSync(p)) throw new Error(`找不到 ${p} —— hash.txt 与 ${SITE_ROOT}/pkg 不同步`);
  return p;
}

async function login() {
  const res = await fetch(`${BASE}/api/graphql`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      query: 'mutation($e: String!, $p: String!) { login(email: $e, password: $p) { token } }',
      variables: { e: 'admin@local', p: 'Admin12345' },
    }),
  });
  const json = await res.json();
  const token = json?.data?.login?.token;
  if (!token) throw new Error(`登录失败：${JSON.stringify(json)}`);
  return token;
}

async function gql(query, variables, token) {
  const res = await fetch(`${BASE}/api/graphql`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
    body: JSON.stringify({ query, variables }),
  });
  const json = await res.json();
  if (json.errors) throw new Error(`GraphQL 报错：${JSON.stringify(json.errors)}`);
  return json.data;
}

// 三章内容：h1 → h2 → h1，之间塞足段落，保证页面可滚动（目录跳转才测得出来）。
const DETAIL = [
  '<h1>第一章 概述</h1>',
  ...Array.from({ length: 12 }, (_, i) => `<p>第一章正文 ${i + 1}</p>`),
  '<h2>第一节 背景</h2>',
  ...Array.from({ length: 12 }, (_, i) => `<p>背景正文 ${i + 1}</p>`),
  '<h1>第二章 方案</h1>',
  ...Array.from({ length: 12 }, (_, i) => `<p>方案正文 ${i + 1}</p>`),
].join('');

async function seed(token) {
  const ws = await gql(
    'mutation($n: String!) { createWorkspace(name: $n) { id slug } }',
    { n: `slash-toc-${Date.now()}` }, token);
  const e = await gql(
    'mutation($id: ID!, $t: String!, $d: String) { createEntry(workspaceId: $id, title: $t, detail: $d) { code } }',
    { id: ws.createWorkspace.id, t: '斜杠与目录', d: DETAIL }, token);
  return { slug: ws.createWorkspace.slug, code: e.createEntry.code };
}

async function newContext(browser, token) {
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  await ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
  const wasm = fs.readFileSync(publishedWasm());
  await ctx.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  return ctx;
}

async function openEntry(page, slug, code) {
  await page.goto(`${BASE}/${slug}/entry/${code}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.tiptap-content', { timeout: 60000 });
  await page.waitForFunction(() => {
    const el = document.querySelector('.tiptap-content');
    return el && el.getAttribute('contenteditable') === 'true';
  }, { timeout: 30000 });
  // 等页面异步数据落地（右栏元信息 + 目录都由 GraphQL 回来后才有）
  await page.waitForSelector('.entry-toc', { timeout: 30000 });
}

/** 重新加载页面：丢掉上一段测试打进去的、未保存的内容，回到种子正文。 */
async function reloadEntry(page, slug, code) {
  await page.goto(`${BASE}/${slug}/entry/${code}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForFunction(() => {
    const el = document.querySelector('.entry-editor .tiptap-content');
    return el && el.getAttribute('contenteditable') === 'true';
  }, { timeout: 30000 });
  await page.waitForTimeout(400);
}

/** 在正文末尾新起一个段落，把光标放进去（斜杠只在块首 / 空白后触发）。 */
async function focusNewParagraph(page) {
  await page.evaluate(() => {
    const el = document.querySelector('.tiptap-content');
    const ed = el.closest('.tiptap-editor').__rodeo_editor;
    ed.chain().focus('end').splitBlock().run();
  });
  await page.waitForTimeout(200);
}

async function main() {
  const token = await login();
  const { slug, code } = await seed(token);
  const browser = await pw.chromium.launch({ executablePath: EXECUTABLE, headless: true });
  const ctx = await newContext(browser, token);
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));

  // ── 4. 全屏页章节目录 ────────────────────────────────────────────
  console.log('\n[4] 全屏页章节目录');
  await openEntry(page, slug, code);
  const toc = await page.evaluate(() => {
    const layout = document.querySelector('.entry-layout');
    const nav = document.querySelector('.entry-toc');
    const main = document.querySelector('.entry-main');
    return {
      withToc: layout.classList.contains('with-toc'),
      items: [...nav.querySelectorAll('.entry-toc-item')].map((b) => ({
        text: b.innerText.trim(),
        level: b.dataset.level,
      })),
      tocLeft: nav.getBoundingClientRect().left,
      mainLeft: main.getBoundingClientRect().left,
    };
  });
  check('有目录时 .entry-layout 带 with-toc', toc.withToc);
  check('目录列出 3 个标题', toc.items.length === 3, JSON.stringify(toc.items));
  check('层级依次是 1/2/1', toc.items.map((i) => i.level).join(',') === '1,2,1',
    toc.items.map((i) => i.level).join(','));
  check('目录标题文本正确', toc.items[0].text === '第一章 概述' && toc.items[2].text === '第二章 方案',
    JSON.stringify(toc.items.map((i) => i.text)));
  check('目录在编辑器左侧', toc.tocLeft < toc.mainLeft, `${toc.tocLeft} < ${toc.mainLeft}`);

  // 点第三章 → 窗口应该滚动、被点的项高亮
  await page.click('.entry-toc-item:nth-child(3)');
  await page.waitForTimeout(700);
  const jumped = await page.evaluate(() => {
    const items = [...document.querySelectorAll('.entry-toc-item')];
    const h1s = [...document.querySelectorAll('.entry-editor .tiptap-content h1')];
    return {
      scrollY: window.scrollY,
      active: items.findIndex((b) => b.classList.contains('on')),
      headingTop: h1s[1] ? h1s[1].getBoundingClientRect().top : null,
    };
  });
  check('点击目录项后窗口滚动', jumped.scrollY > 100, `scrollY=${jumped.scrollY}`);
  check('第二章被滚到视口上部', jumped.headingTop !== null && jumped.headingTop > 0 && jumped.headingTop < 260,
    `top=${jumped.headingTop}`);
  check('被点的目录项高亮', jumped.active === 2, `active=${jumped.active}`);

  // ── 5. 斜杠命令 ─────────────────────────────────────────────────
  console.log('\n[5] 斜杠命令');
  await page.evaluate(() => window.scrollTo(0, 0));
  await focusNewParagraph(page);
  await page.keyboard.type('/');
  await page.waitForSelector('.tiptap-slash', { timeout: 5000 });
  const slashAll = await page.$$eval('.tiptap-slash-item .tiptap-slash-label', (els) =>
    els.map((e) => e.innerText.trim()));
  check('斜杠菜单弹出', slashAll.length >= 10, `items=${slashAll.length}`);
  check('包含标题 / 列表 / 代码块 / 表格 / 图片',
    ['标题 1', '无序列表', '代码块', '表格', '图片'].every((x) => slashAll.includes(x)),
    slashAll.join(','));

  await page.keyboard.type('表');
  await page.waitForTimeout(300);
  const filtered = await page.$$eval('.tiptap-slash-item .tiptap-slash-label', (els) =>
    els.map((e) => e.innerText.trim()));
  // 「表」会同时命中「列表」类命令的关键词，所以不要求只剩一条，
  // 但按相关度排序后「表格」必须排第一（回车默认执行的就是它）。
  check('输入「表」后表格排第一', filtered[0] === '表格', JSON.stringify(filtered));

  await page.keyboard.press('Enter');
  await page.waitForTimeout(500);
  const hasTable = await page.evaluate(() =>
    document.querySelectorAll('.entry-editor .tiptap-content table').length);
  check('回车插入表格', hasTable === 1, `tables=${hasTable}`);
  check('执行后菜单关闭', await page.$('.tiptap-slash') === null);

  // 标题：新段落里 /h1（重新加载，免得表格是最后一个块，focus('end') 落进表格里）
  await reloadEntry(page, slug, code);
  await focusNewParagraph(page);
  await page.keyboard.type('/h1');
  await page.waitForTimeout(300);
  const h1Filtered = await page.$$eval('.tiptap-slash-item .tiptap-slash-label', (els) =>
    els.map((e) => e.innerText.trim()));
  check('输入 h1 过滤到标题 1', h1Filtered.length === 1 && h1Filtered[0] === '标题 1', JSON.stringify(h1Filtered));
  await page.keyboard.press('Enter');
  await page.waitForTimeout(300);
  await page.keyboard.type('斜杠生成的标题');
  const heading = await page.evaluate(() => {
    const hs = [...document.querySelectorAll('.entry-editor .tiptap-content h1')];
    return hs.map((h) => h.innerText.trim());
  });
  check('回车把段落变成一级标题', heading.includes('斜杠生成的标题'), JSON.stringify(heading));

  // Esc 关闭，正文保留 `/`（同样重新加载，保证最后一个块不是表格）
  await reloadEntry(page, slug, code);
  await focusNewParagraph(page);
  await page.keyboard.type('/');
  await page.waitForSelector('.tiptap-slash', { timeout: 5000 });
  await page.keyboard.press('Escape');
  await page.waitForTimeout(200);
  check('Esc 关闭斜杠菜单', await page.$('.tiptap-slash') === null);
  const kept = await page.evaluate(() => {
    const el = document.querySelector('.entry-editor .tiptap-content');
    return el.innerText.includes('/');
  });
  check('Esc 后 `/` 仍留在正文里', kept);

  // 正文中间的斜杠不触发（不是块首 / 空白之后）
  await page.keyboard.type('/');
  await page.waitForTimeout(400);
  check('连续两个斜杠不重复弹菜单', await page.$('.tiptap-slash') === null);

  check('页面无 JS 错误', errors.length === 0, errors.join(' | '));

  // ── 6. 新建按钮 hover 菜单 ──────────────────────────────────────
  console.log('\n[6] 新建按钮 hover 菜单');
  await page.goto(`${BASE}/${slug}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.newentry-btn', { timeout: 60000 });
  await page.hover('.newentry-btn');
  await page.waitForSelector('.newentry-split', { state: 'visible', timeout: 5000 });
  const boxes = await page.evaluate(() => {
    const b = document.querySelector('.newentry-btn').getBoundingClientRect();
    const s = document.querySelector('.newentry-split').getBoundingClientRect();
    return { bx: b.x + b.width / 2, by: b.y + b.height / 2, sx: s.x + s.width / 2, sy: s.y + s.height / 2, gap: s.top - b.bottom };
  });
  // 真实鼠标轨迹：从按钮中心分多步移到菜单中心，中途必须穿过那几像素缝隙。
  await page.mouse.move(boxes.bx, boxes.by);
  await page.mouse.move(boxes.sx, boxes.sy, { steps: 12 });
  await page.waitForTimeout(200);
  const stillOpen = await page.evaluate(() => {
    const s = document.querySelector('.newentry-split');
    if (!s) return { visible: false, hit: null };
    const r = s.getBoundingClientRect();
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + 20);
    // 菜单项里是 <span>，往上找最近的按钮才是「可点击」的证据。
    const btn = hit && hit.closest ? hit.closest('button') : null;
    return { visible: getComputedStyle(s).display !== 'none', hit: btn ? 'button' : hit ? hit.tagName : null };
  });
  check('穿过按钮与菜单之间的缝隙后菜单仍在', stillOpen.visible,
    `gap=${boxes.gap}px, hit=${stillOpen.hit}`);
  check('菜单项可以命中', String(stillOpen.hit || '').toLowerCase().startsWith('button'), String(stillOpen.hit));

  await browser.close();
  console.log(`\n${failures.length ? `FAILED (${failures.length}): ${failures.join(', ')}` : 'ALL PASS'}`);
  process.exit(failures.length ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
