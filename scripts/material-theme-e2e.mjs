// Material 主题浏览器验证脚本（playwright-core）。
//
// 用法：
//   1) 构建：LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build
//   2) 起服务：LEPTOS_SITE_ADDR=127.0.0.1:3099 LEPTOS_SITE_ROOT=target/site-smoke \
//              LEPTOS_HASH_FILES=true LEPTOS_OUTPUT_NAME=rodeo \
//              ./target/debug/rodeo /tmp/material-smoke.toml
//   3) 验证：node scripts/material-theme-e2e.mjs
//
// 踩过的坑（来自 scripts/editor-smoke.mjs 的经验 + 本轮实测）：
//   - playwright-core 是 CommonJS，NODE_PATH 与 ESM 具名 import 都不 work，
//     只能按绝对路径 default import。
//   - debug wasm 约 37MB，gzip 下 CompressionLayer 会卡住 >60s；用
//     page.route('**/pkg/*.wasm') 从磁盘 fulfill。
//   - hash-files 会留好几代 hashed 文件，按 target/debug/hash.txt 精确匹配。
//   - localStorage.rodeo_jwt 是 JSON 编码过的字符串（带引号）。
//   - hydration 报错只走 console.error，只挂 pageerror 的脚本会「全过」
//     而页面其实是死的 —— 必须监听 console。
//   - 主题按钮的 color 有 120ms 的 transition，读计算样式前要等它走完，
//     否则读到的是过渡中的插值。
//   - 预置 localStorage.rodeo_theme 现在【是安全的】：主题按钮的两枚图标
//     永远都渲染在 DOM 里、显示哪一枚由 CSS 按 data-theme 决定，SSR 与
//     hydrate 的 DOM 结构因此完全一致，不再有子树失配。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.RODEO_BASE || 'http://127.0.0.1:3099';
const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const SITE_ROOT = process.env.RODEO_SITE_ROOT || 'target/site-smoke';
const SHOTS = path.join(REPO, process.env.RODEO_SHOTS || 'target/material-shots');
const EXECUTABLE = path.join(
  process.env.HOME,
  'Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64',
  'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
);

let failures = [];
function check(name, ok, detail) {
  if (ok) console.log(`  ok   ${name}`);
  else { console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`); failures.push(name); }
}
function eq(name, actual, expected) {
  check(name, actual === expected, `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

function publishedWasm() {
  const hashFile = path.join(REPO, 'target/debug/hash.txt');
  if (!fs.existsSync(hashFile)) throw new Error('找不到 target/debug/hash.txt —— 先跑 cargo leptos build');
  const hash = fs.readFileSync(hashFile, 'utf8').split('\n')
    .find((l) => l.startsWith('wasm:'))?.split(/[:\s]+/)[1];
  if (!hash) throw new Error('target/debug/hash.txt 里没有 wasm 哈希');
  const p = path.join(REPO, SITE_ROOT, 'pkg', `rodeo.${hash}.wasm`);
  if (!fs.existsSync(p)) throw new Error(`找不到 ${p} —— hash.txt 与 ${SITE_ROOT}/pkg 不同步，重跑 cargo leptos build`);
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

async function seed(token) {
  const ws = await gql('mutation($n: String!) { createWorkspace(name: $n) { id slug } }',
    { n: `material-${Date.now()}` }, token);
  await gql('mutation($id: ID!, $t: String!) { createEntry(workspaceId: $id, title: $t) { code } }',
    { id: ws.createWorkspace.id, t: 'Material 验证条目' }, token);
  return ws.createWorkspace.slug;
}

// 页面上 <html> 上的两个属性 + localStorage 里的持久化值
async function themeState(page) {
  return page.evaluate(() => ({
    theme: document.documentElement.getAttribute('data-theme'),
    skin: document.documentElement.getAttribute('data-skin'),
    store: localStorage.getItem('rodeo_theme'),
  }));
}

// 主题按钮当前显示的是哪一枚图标（CSS 按 data-theme 决定）
async function visibleIcon(page) {
  return page.evaluate(() => {
    const btn = document.querySelector('.appbar .theme-btn');
    if (!btn) return null;
    const vis = (sel) => {
      const el = btn.querySelector(sel);
      if (!el) return false;
      const r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    };
    return { sun: vis('.ti-sun'), moon: vis('.ti-moon') };
  });
}

async function run() {
  if (!fs.existsSync(EXECUTABLE)) throw new Error(`找不到 Chromium：${EXECUTABLE}`);
  fs.mkdirSync(SHOTS, { recursive: true });
  const token = await login();
  const slug = await seed(token);
  const wasm = fs.readFileSync(publishedWasm());

  const browser = await pw.chromium.launch({ executablePath: EXECUTABLE, headless: true });

  const consoleErrors = [];
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  await ctx.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  await ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);

  const page = await ctx.newPage();
  page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('pageerror', (e) => consoleErrors.push(`pageerror: ${e.message}`));

  // ---- 1. 首屏：没有存过主题时按系统偏好解析成经典亮/暗 ----
  await page.goto(`${BASE}/${slug}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await page.waitForTimeout(600);
  const first = await themeState(page);
  check('首屏写入了 data-theme', first.theme === 'light' || first.theme === 'dark', JSON.stringify(first));
  eq('首屏默认经典皮肤', first.skin, 'classic');
  check('首屏把主题固化进了 localStorage',
    ['light', 'dark'].includes(first.store), JSON.stringify(first));

  // ---- 2. 四态循环 ----
  // 期望值从【观测到的起始态】推出来，而不是写死「第一次点击后应该是亮色」。
  // 起始态取决于机器的 prefers-color-scheme（headless Chrome 默认 light），
  // 写死会让整个序列错位一格 —— 而四态循环本身是对的。
  const CYCLE = [
    { theme: 'light', skin: 'classic', store: 'light' },
    { theme: 'dark', skin: 'classic', store: 'dark' },
    { theme: 'light', skin: 'material', store: 'material-light' },
    { theme: 'dark', skin: 'material', store: 'material-dark' },
  ];
  const startIdx = CYCLE.findIndex((c) => c.theme === first.theme && c.skin === first.skin);
  check('起始态在四态循环里', startIdx >= 0, JSON.stringify(first));
  console.log(`  起始态：${first.theme}/${first.skin}（循环下标 ${startIdx}）`);

  for (let i = 0; i < 4; i++) {
    await page.click('.appbar .theme-btn');
    await page.waitForTimeout(350);   // 等 color 的 120ms transition 走完
    const st = await themeState(page);
    // 第 i 次点击（0 起）后应落在 CYCLE[(startIdx + i + 1) % 4]
    const exp = CYCLE[(startIdx + i + 1) % 4];
    eq(`第 ${i + 1} 次点击 → data-theme`, st.theme, exp.theme);
    eq(`第 ${i + 1} 次点击 → data-skin`, st.skin, exp.skin);
    eq(`第 ${i + 1} 次点击 → localStorage`, st.store, exp.store);

    const ic = await visibleIcon(page);
    const want = exp.theme === 'light' ? 'sun' : 'moon';
    check(`第 ${i + 1} 次点击 → 图标只剩一枚且是${want === 'sun' ? '太阳' : '月亮'}`,
      ic && (ic.sun !== ic.moon) && ic[want] === true, JSON.stringify(ic));
  }

  // ---- 3. 刷新后状态保持（四次点击后应回到起始态）----
  const backToStart = CYCLE[startIdx];
  await page.reload({ waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await page.waitForTimeout(600);
  const after = await themeState(page);
  eq('刷新后 data-theme 保持', after.theme, backToStart.theme);
  eq('刷新后 data-skin 保持', after.skin, backToStart.skin);
  eq('刷新后 localStorage 保持', after.store, backToStart.store);

  // ---- 4. 旧值 system 的迁移（单独一个 context） ----
  const ctx2 = await browser.newContext({ ignoreHTTPSErrors: true });
  await ctx2.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  await ctx2.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
  await ctx2.addInitScript(() => { localStorage.setItem('rodeo_theme', 'system'); });
  const p2 = await ctx2.newPage();
  const errs2 = [];
  p2.on('console', (m) => { if (m.type() === 'error') errs2.push(m.text()); });
  p2.on('pageerror', (e) => errs2.push(`pageerror: ${e.message}`));
  await p2.goto(`${BASE}/${slug}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await p2.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await p2.waitForTimeout(600);
  const migrated = await themeState(p2);
  check('旧值 system 被解析成具体值',
    ['light', 'dark'].includes(migrated.store), JSON.stringify(migrated));
  eq('迁移后仍是经典皮肤', migrated.skin, 'classic');
  check('迁移过程无 console.error', errs2.length === 0, errs2.slice(0, 3).join(' | '));

  // ---- 5. 预置 material-light 不引发 hydration 报错（图标 CSS 化的直接收益） ----
  const ctx3 = await browser.newContext({ ignoreHTTPSErrors: true });
  await ctx3.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  await ctx3.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
  await ctx3.addInitScript(() => { localStorage.setItem('rodeo_theme', 'material-light'); });
  const p3 = await ctx3.newPage();
  const errs3 = [];
  p3.on('console', (m) => { if (m.type() === 'error') errs3.push(m.text()); });
  p3.on('pageerror', (e) => errs3.push(`pageerror: ${e.message}`));
  await p3.goto(`${BASE}/${slug}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await p3.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
  await p3.waitForTimeout(600);
  const pre = await themeState(p3);
  eq('预置 material-light → data-skin', pre.skin, 'material');
  eq('预置 material-light → data-theme', pre.theme, 'light');
  check('预置非默认主题无 console.error', errs3.length === 0, errs3.slice(0, 3).join(' | '));
  await ctx2.close();
  await ctx3.close();

  // ---- 6. 各界面截图（Material 亮 / Material 暗 / 经典亮 / 经典暗） ----
  const pages = [
    ['workspaces', `${BASE}/workspaces`],
    ['workspace', `${BASE}/${slug}`],
    ['settings', `${BASE}/${slug}/settings`],
    ['login', `${BASE}/login`],
  ];
  for (const mode of ['material-light', 'material-dark', 'light', 'dark']) {
    await page.evaluate((m) => localStorage.setItem('rodeo_theme', m), mode);
    for (const [name, url] of pages) {
      await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 120000 });
      await page.waitForTimeout(1400);
      const file = path.join(SHOTS, `${name}-${mode}.png`);
      await page.screenshot({ path: file, fullPage: true });
      console.log(`  截图 ${path.relative(REPO, file)}`);
    }
  }

  // ---- 7. 全程无 console.error（hydration 报错检测） ----
  check('主流程无 console.error', consoleErrors.length === 0,
    consoleErrors.slice(0, 5).join(' | '));

  await browser.close();

  console.log('');
  if (failures.length) {
    console.log(`✗ ${failures.length} 项失败：`);
    failures.forEach((f) => console.log(`  - ${f}`));
    process.exit(1);
  }
  console.log('✓ 全部通过');
}

run().catch((e) => { console.error(e); process.exit(1); });
