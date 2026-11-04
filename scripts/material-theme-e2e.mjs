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

// ---- 对比度（WCAG 2.x 相对亮度）----
function srgb(v) {
  const s = v / 255;
  return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}
function parseRgb(css) {
  const m = (css || '').match(/rgba?\(([^)]+)\)/);
  if (!m) return null;
  const p = m[1].split(/[\s,/]+/).filter(Boolean).map(Number);
  return p.length >= 3 ? [p[0], p[1], p[2]] : null;
}
function contrastRatio(fgCss, bgCss) {
  const fg = parseRgb(fgCss);
  const bg = parseRgb(bgCss);
  if (!fg || !bg) return null;
  const lum = (c) => 0.2126 * srgb(c[0]) + 0.7152 * srgb(c[1]) + 0.0722 * srgb(c[2]);
  const [hi, lo] = [lum(fg), lum(bg)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
}

// 元素的实际计算样式（前景色 + 背景色）
async function measurePair(page, selector) {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return null;
    const cs = getComputedStyle(el);
    return { color: cs.color, bg: cs.backgroundColor };
  }, selector);
}

// hover 态没法用真实鼠标稳定命中，改用 CDP 强制伪类：
// 造一个带目标 class 的探针元素挂到 DOM 上，强制 :hover，再读它的计算样式。
// 读出来的是真实层叠结果（Material 的覆盖规则与经典规则谁赢，一并验证到）。
async function measureHover(page, cdp, cls) {
  await page.evaluate((c) => {
    document.querySelectorAll('.probe-hover').forEach((e) => e.remove());
    const el = document.createElement('span');
    el.className = `${c} probe-hover`;
    el.style.cssText = 'position:fixed;left:-9999px;top:0;width:10px;height:10px;';
    document.body.appendChild(el);
  }, cls);
  await cdp.send('DOM.enable');
  await cdp.send('CSS.enable');
  const { root } = await cdp.send('DOM.getDocument');
  const { nodeId } = await cdp.send('DOM.querySelector', {
    nodeId: root.nodeId, selector: '.probe-hover',
  });
  if (!nodeId) return null;
  await cdp.send('CSS.forcePseudoState', { nodeId, forcedPseudoClasses: ['hover'] });
  const out = await page.evaluate(() => {
    const cs = getComputedStyle(document.querySelector('.probe-hover'));
    return { color: cs.color, bg: cs.backgroundColor };
  });
  await cdp.send('CSS.forcePseudoState', { nodeId, forcedPseudoClasses: [] });
  await page.evaluate(() =>
    document.querySelectorAll('.probe-hover').forEach((e) => e.remove()));
  return out;
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
  // 用于强制 :hover 伪类，测出 hover 态的真实层叠结果
  const cdp = await ctx.newCDPSession(page);

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

  // ---- 6. 语义色按钮的对比度（浏览器实测计算样式） ----
  // 经典版这几处是「语义色背景 + 写死的 #fff 文字」。Material 暗色下 --pos
  // 变成浅绿 #6FDD8B、--dgr 变成浅红 #FFB4AB，白字压上去只有 1.9:1，几乎
  // 看不见。Material 的覆盖把它们换成了主色/错误色配对。
  //
  // 经典两项断言的是【绝对色值】：Material 的改动一行都不许漏进经典皮肤。
  // （顺带记录：经典暗色下白字压浅绿只有 2.5:1，是既有行为，本任务不改。）
  const CONTRAST = [
    { theme: 'light', btn: ['rgb(255, 255, 255)', 'rgb(26, 127, 55)'], hover: ['rgb(255, 255, 255)', 'rgb(207, 34, 46)'] },
    { theme: 'dark', btn: ['rgb(255, 255, 255)', 'rgb(63, 185, 80)'], hover: ['rgb(255, 255, 255)', 'rgb(248, 81, 73)'] },
    { theme: 'material-light', min: 4.5 },
    { theme: 'material-dark', min: 4.5 },
  ];
  for (const c of CONTRAST) {
    await page.evaluate((m) => localStorage.setItem('rodeo_theme', m), c.theme);
    await page.reload({ waitUntil: 'domcontentloaded', timeout: 120000 });
    await page.waitForSelector('.appbar .theme-btn', { timeout: 60000 });
    await page.waitForTimeout(400);

    const btn = await measurePair(page, '.newentry-btn');
    check(`${c.theme}：新建按钮存在`, btn !== null, JSON.stringify(btn));
    const btnRatio = btn ? contrastRatio(btn.color, btn.bg) : null;
    const hov = await measureHover(page, cdp, 'agent-attach-chip-x');
    check(`${c.theme}：删除按钮 hover 态可测`, hov !== null, JSON.stringify(hov));
    const hovRatio = hov ? contrastRatio(hov.color, hov.bg) : null;

    if (c.btn) {
      // 经典：色值必须逐字节不变
      eq(`${c.theme}：新建按钮前景色未变`, btn.color, c.btn[0]);
      eq(`${c.theme}：新建按钮底色未变`, btn.bg, c.btn[1]);
      eq(`${c.theme}：删除按钮 hover 前景色未变`, hov.color, c.hover[0]);
      eq(`${c.theme}：删除按钮 hover 底色未变`, hov.bg, c.hover[1]);
      console.log(`  ${c.theme}  新建按钮 ${btnRatio.toFixed(2)}:1（经典既有值，保持不变）`);
    } else {
      check(`${c.theme}：新建按钮文字对比度 ≥ ${c.min}`,
        btnRatio !== null && btnRatio >= c.min,
        `${btn?.color} on ${btn?.bg} = ${btnRatio?.toFixed(2)}`);
      console.log(`  ${c.theme}  新建按钮 ${btnRatio.toFixed(2)}:1`);
    }
    // hover 态是 ✕ 图标（图形对象），按 WCAG 1.4.11 要求 ≥ 3:1
    check(`${c.theme}：删除按钮 hover 对比度 ≥ 3`,
      hovRatio !== null && hovRatio >= 3,
      `${hov?.color} on ${hov?.bg} = ${hovRatio?.toFixed(2)}`);
  }

  // ---- 7. 各界面截图（Material 亮 / Material 暗 / 经典亮 / 经典暗） ----
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

  // ---- 8. 全程无 console.error（hydration 报错检测） ----
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
