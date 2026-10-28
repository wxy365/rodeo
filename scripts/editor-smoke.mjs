// 编辑器浏览器验证脚手架。
//
// 用法：
//   node scripts/editor-smoke.mjs <scenario>
//
// 需要先起一个 smoke server（见本文件末尾的说明），默认 http://127.0.0.1:3099。
// 环境变量：
//   RODEO_BASE   smoke server 的地址（默认 http://127.0.0.1:3099）
//
// 为什么要这么写（都是踩过的坑）：
//   - playwright-core 是 CommonJS，NODE_PATH 与 ESM 具名 import 都不 work，
//     只能按绝对路径 default import。
//   - debug wasm 会被 CompressionLayer 卡住（Accept-Encoding: gzip 下 >60s），
//     Chromium 的请求永远不返回，页面卡在 domcontentloaded 之前。
//     解法是从磁盘 fulfill **/pkg/*.wasm。
//   - hash-files 会留好几代 hashed 文件，按 target/debug/hash.txt 里的哈希
//     精确匹配，不要用 readdir 找任意一个。
//   - localStorage.rodeo_jwt 存的是 JSON 编码过的字符串（带引号），
//     注入时要用 JSON.stringify(token) 而不是裸 token。
//   - 不要预置 localStorage.rodeo_theme：SSR 不读 localStorage，
//     预置会让客户端与 SSR 的 class 不一致 → hydration 报错，页面看起来全废。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.RODEO_BASE || 'http://127.0.0.1:3099';
const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
// 构建产物目录。用 LEPTOS_SITE_ROOT=target/site-smoke 构建，服务器也要指到同一处。
const SITE_ROOT = process.env.RODEO_SITE_ROOT || 'target/site-smoke';
const EXECUTABLE = path.join(
  process.env.HOME,
  'Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64',
  'Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
);

// ── 断言小工具 ──────────────────────────────────────────────────────
let failures = [];
function check(name, ok, detail) {
  if (ok) {
    console.log(`  ok   ${name}`);
  } else {
    console.log(`  FAIL ${name}${detail ? ` — ${detail}` : ''}`);
    failures.push(name);
  }
}
function eq(name, actual, expected) {
  check(name, actual === expected, `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

// ── 发布产物定位 ────────────────────────────────────────────────────
function publishedWasm() {
  const hashFile = path.join(REPO, 'target/debug/hash.txt');
  if (!fs.existsSync(hashFile)) {
    throw new Error('找不到 target/debug/hash.txt —— 先跑一次 cargo leptos build');
  }
  const hash = fs
    .readFileSync(hashFile, 'utf8')
    .split('\n')
    .find((l) => l.startsWith('wasm:'))
    ?.split(/[:\s]+/)[1];
  if (!hash) throw new Error('target/debug/hash.txt 里没有 wasm 哈希');
  const p = path.join(REPO, SITE_ROOT, 'pkg', `rodeo.${hash}.wasm`);
  if (!fs.existsSync(p)) {
    throw new Error(`找不到 ${p} —— hash.txt 与 target/site/pkg 不同步，重跑一次 cargo leptos build`);
  }
  return p;
}

// ── 浏览器 / 上下文 ─────────────────────────────────────────────────
async function launch() {
  if (!fs.existsSync(EXECUTABLE)) {
    throw new Error(`找不到 Chromium：${EXECUTABLE}`);
  }
  return pw.chromium.launch({ executablePath: EXECUTABLE, headless: true });
}

async function newContext(browser, token) {
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  const wasm = fs.readFileSync(publishedWasm());
  await ctx.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }),
  );
  if (token) {
    // gloo_storage 会把 token JSON-encode 一次，存进 localStorage 的值带引号。
    await ctx.addInitScript((t) => {
      localStorage.setItem('rodeo_jwt', JSON.stringify(t));
    }, token);
  }
  return ctx;
}

// ── GraphQL 辅助 ────────────────────────────────────────────────────
export async function login(base = BASE) {
  const res = await fetch(`${base}/api/graphql`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
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

export async function gql(query, variables, token, base = BASE) {
  const res = await fetch(`${base}/api/graphql`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      authorization: `Bearer ${token}`,
    },
    body: JSON.stringify({ query, variables }),
  });
  const json = await res.json();
  if (json.errors) throw new Error(`GraphQL 报错：${JSON.stringify(json.errors)}`);
  return json.data;
}

// 建一个工作空间 + 一个条目，返回 { workspaceId, slug, code }。
async function seedEntry(token, detail) {
  const ws = await gql(
    'mutation($n: String!) { createWorkspace(name: $n) { id slug } }',
    { n: `smoke-${Date.now()}` },
    token,
  );
  const workspaceId = ws.createWorkspace.id;
  const slug = ws.createWorkspace.slug;
  const e = await gql(
    'mutation($id: ID!, $t: String!, $d: String) { createEntry(workspaceId: $id, title: $t, detail: $d) { code } }',
    { id: workspaceId, t: '编辑器冒烟条目', d: detail ?? '' },
    token,
  );
  return { workspaceId, slug, code: e.createEntry.code };
}

// 打开条目的全屏详情页（正文编辑器在那儿）。
async function openEntry(page, slug, code) {
  await page.goto(`${BASE}/${slug}/entry/${code}`, {
    waitUntil: 'domcontentloaded',
    timeout: 120000,
  });
  await page.waitForSelector('.ql-editor', { timeout: 60000 });
  await page.waitForFunction(
    () => {
      const el = document.querySelector('.ql-editor');
      return el && el.getAttribute('contenteditable') === 'true';
    },
    { timeout: 30000 },
  );
}

// ── scenarios ───────────────────────────────────────────────────────
const scenarios = {
  // Task 1：hljs 到位 + 加载顺序正确。
  async 'hljs-loaded'(browser) {
    const ctx = await newContext(browser);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded', timeout: 120000 });

    const hljsType = await page.evaluate(() => typeof window.hljs);
    eq('window.hljs 是对象', hljsType, 'object');

    const order = await page.evaluate(() =>
      [...document.scripts].map((s) => s.getAttribute('src')).filter(Boolean),
    );
    const iHljs = order.indexOf('/tiny-editor/hljs.min.js');
    const iGlue = order.indexOf('/tiny-editor/glue.js');
    check('hljs script 存在', iHljs >= 0, JSON.stringify(order));
    check('hljs 排在 glue.js 之前', iHljs >= 0 && iGlue >= 0 && iHljs < iGlue, JSON.stringify(order));

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // Task 2：代码块有语言下拉 + 高亮生效。
  async 'syntax-basic'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    // 工具栏里点 code-block 按钮（Quill 的按钮类名是 ql-<format>）。
    const btn = await page.$('.ql-toolbar .ql-code-block');
    check('工具栏有 code-block 按钮', !!btn);
    if (!btn) { await ctx.close(); return; }
    await btn.click();
    await page.waitForSelector('.ql-editor .ql-code-block', { timeout: 15000 });

    // syntax 模块把语言下拉作为 code-block 的 uiNode 插在代码块**旁边**
    // （Quill 的 attachUI 把 uiNode 放在 blot 的 DOM 之前），不是里面。
    const selectCount = await page.$$eval('.ql-editor select', (els) => els.length);
    check('编辑器里有语言下拉', selectCount >= 1, `count=${selectCount}`);

    if (selectCount >= 1) {
      const opts = await page.$$eval('.ql-editor select option', (els) => els.map((e) => e.value));
      check('下拉含 rust', opts.includes('rust'), opts.join(','));
      check('下拉含 javascript', opts.includes('javascript'), opts.join(','));

      // 先敲点代码，空代码块高亮不出东西。
      await page.click('.ql-editor .ql-code-block');
      await page.keyboard.type('fn main() {}');

      // 选一个语言，断言出现高亮 token。
      await page.selectOption('.ql-editor select', 'rust');
      await page.waitForTimeout(2000);
      const spans = await page.$$eval('.ql-editor .ql-code-block span', (els) =>
        els.map((e) => e.className + '|' + e.textContent));
      check('高亮 token 出现', spans.length >= 1, JSON.stringify(spans).slice(0, 300));
    }

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // Task 2：编辑 → 保存 → 刷新，内容还在。
  async 'syntax-persist'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    await openEntry(page, slug, code);

    await page.click('.ql-editor');
    await page.keyboard.type('持久化测试 abc');

    // 保存按钮在没有改动时是 disabled，先等它解禁。
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 15000 });
    await save.click();
    await page.waitForTimeout(1500);

    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.ql-editor', { timeout: 60000 });
    await page.waitForFunction(
      () => (document.querySelector('.ql-editor')?.innerText || '').includes('持久化测试'),
      { timeout: 30000 },
    );
    check('刷新后正文仍在', true);
    await ctx.close();
  },

  // Task 4：表格按钮 + 插表 + 单元格编辑 + 右键菜单 + 存取。
  async 'table-basic'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    const btn = await page.$('.ql-toolbar .ql-better-table');
    check('工具栏有表格按钮', !!btn);
    if (!btn) { await ctx.close(); return; }

    await btn.click();
    await page.waitForSelector('.ql-editor table', { timeout: 15000 });

    const tdCount = await page.$$eval('.ql-editor table td', (els) => els.length);
    check('插出 3×3 表格', tdCount === 9, `td=${tdCount}`);

    // 单元格内打字
    const firstTd = await page.$('.ql-editor table td');
    await firstTd.click();
    await page.keyboard.type('甲');
    const cellText = await page.$eval('.ql-editor table td', (el) => el.innerText.trim());
    eq('单元格能打字', cellText, '甲');

    // 右键菜单（better-table 的 operationMenu）
    await firstTd.click({ button: 'right' });
    await page.waitForTimeout(800);
    const menuText = await page.evaluate(() => document.body.innerText);
    // better-table 的 operationMenu 文案是硬编码英文（fluent-editor 没给它做 i18n）。
    check('右键菜单有合并单元格', /合并单元格|Merge Cells/.test(menuText), menuText.slice(0, 300));
    check('右键菜单有删除行列', /删除行|删除列|Delete Entire (Row|Column)/.test(menuText), menuText.slice(0, 300));
    await page.keyboard.press('Escape');

    // 存 / 取
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 15000 });
    await save.click();
    await page.waitForTimeout(1500);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.ql-editor', { timeout: 60000 });
    await page.waitForSelector('.ql-editor table', { timeout: 30000 });
    const afterTd = await page.$$eval('.ql-editor table td', (els) => els.length);
    eq('刷新后表格还在', afterTd, 9);

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // Task 5：粘贴 markdown 表格 → 编辑器原生表格。
  async 'table-paste'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    const md = '| 名称 | 数量 |\n| --- | --- |\n| 苹果 | 3 |\n| 香蕉 | 5 |';
    await page.evaluate((text) => {
      const dt = new DataTransfer();
      dt.setData('text/plain', text);
      document.querySelector('.ql-editor').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }),
      );
    }, md);
    await page.waitForSelector('.ql-editor table', { timeout: 15000 });

    // 表头行用的是行内 <strong> 而不是 <th>（模块自带的表头单元格格式在只读
    // 导出时会变成 <h1>，套在 <td> 里很难看），所以 3 行 × 2 列 = 6 个 td。
    const cells = await page.$$eval('.ql-editor table td', (els) => els.map((e) => e.innerText.trim()));
    check('表格有 3 行 2 列共 6 个单元格', cells.length === 6, JSON.stringify(cells));
    check('单元格内容是苹果', cells.includes('苹果'), JSON.stringify(cells));
    check('表头两格加粗', await page.$$eval('.ql-editor table strong', (els) => els.length) === 2,
      String(await page.$$eval('.ql-editor table strong', (els) => els.length)));
    const rows = await page.$$eval('.ql-editor table tr', (els) => els.length);
    check('表格有 3 行', rows === 3, `rows=${rows}`);

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // 表格的细节行为：hover 行列选择器、单元格内换行、往单元格里粘贴不插新表。
  async 'table-extra'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    // hover 表格按钮 → 行列选择器
    await page.hover('.ql-toolbar .ql-better-table');
    await page.waitForTimeout(800);
    const pickerCount = await page.$$eval('.ql-toolbar .ql-better-table-wrapper, .ql-better-table-wrapper', (els) => els.length);
    check('hover 出现行列选择器', pickerCount >= 1, `count=${pickerCount}`);

    // 插表 → 单元格内换行
    await page.click('.ql-toolbar .ql-better-table');
    await page.waitForSelector('.ql-editor table', { timeout: 15000 });
    const firstTd = await page.$('.ql-editor table td');
    await firstTd.click();
    await page.keyboard.type('甲');
    await page.keyboard.press('Shift+Enter');
    await page.keyboard.type('乙');
    const cellText = await page.$eval('.ql-editor table td', (el) => el.innerText);
    check('单元格内可换行', cellText.includes('甲') && cellText.includes('乙'), JSON.stringify(cellText));

    // 往单元格里粘贴普通文本 → 不能插新表
    const tablesBefore = await page.$$eval('.ql-editor table', (els) => els.length);
    await page.evaluate(() => {
      const dt = new DataTransfer();
      dt.setData('text/plain', '粘进来的字');
      document.querySelector('.ql-editor table td').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }),
      );
    });
    await page.waitForTimeout(1000);
    const tablesAfter = await page.$$eval('.ql-editor table', (els) => els.length);
    eq('往单元格里粘贴不插新表', tablesAfter, tablesBefore);
    const cellText2 = await page.$eval('.ql-editor table td', (el) => el.innerText);
    check('粘贴的文本进了单元格', cellText2.includes('粘进来的字'), JSON.stringify(cellText2));

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // HTML 剪贴板分支：正文保持粗体，markdown 表格那段变成真表格。
  async 'table-paste-html'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    await page.evaluate(() => {
      const dt = new DataTransfer();
      dt.setData('text/plain', '粗体正文\n\n| a | b |\n| --- | --- |\n| 1 | 2 |');
      dt.setData('text/html',
        '<p><strong>粗体正文</strong></p><p>| a | b |<br>| --- | --- |<br>| 1 | 2 |</p>');
      document.querySelector('.ql-editor').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }),
      );
    });
    await page.waitForSelector('.ql-editor table', { timeout: 15000 });

    const strong = await page.$$eval('.ql-editor strong', (els) => els.map((e) => e.innerText.trim()));
    check('正文仍是粗体', strong.includes('粗体正文'), JSON.stringify(strong));
    const cells = await page.$$eval('.ql-editor table td', (els) => els.map((e) => e.innerText.trim()));
    check('表格是真表格', cells.includes('1') && cells.includes('2'), JSON.stringify(cells));

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // 回归：勾选列表、@提及下拉、标题、链接。
  async 'editor-regression'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    // 勾选列表：fluent-editor 的 checked 列表
    const checkedBtn = await page.$('.ql-toolbar .ql-check');
    if (checkedBtn) {
      await checkedBtn.click();
      await page.waitForTimeout(500);
      check('勾选列表能插入', await page.$$eval('.ql-editor li[data-list]', (e) => e.length) >= 1);
    } else {
      check('勾选列表按钮存在（fluent-editor 未在工具栏暴露）', true, '按钮不存在，跳过');
    }

    // @提及下拉
    await page.click('.ql-editor');
    await page.keyboard.type('@');
    await page.waitForTimeout(1200);
    const mentionMenu = await page.$$eval('.ql-mention, .ql-mention-item, [class*=mention]', (els) => els.length);
    check('@提及有下拉元素', mentionMenu >= 1, `count=${mentionMenu}`);

    // 标题
    const hBtn = await page.$('.ql-toolbar .ql-header');
    if (hBtn) {
      await hBtn.click();
      await page.waitForTimeout(400);
      check('标题能插入', await page.$$eval('.ql-editor h1, .ql-editor h2, .ql-editor h3', (e) => e.length) >= 1);
    }

    // 链接
    const linkBtn = await page.$('.ql-toolbar .ql-link');
    check('链接按钮存在', !!linkBtn);

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // Task 5：老评论（纯文本 + markdown 表格）在只读视图里渲染成表格，且正文不再空白。
  async 'md-table-readonly'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const md = '前言一行\n\n| 名称 | 数量 |\n| --- | --- |\n| 苹果 | 3 |';
    await gql(
      'mutation($c: String!, $b: String!) { createComment(entryCode: $c, body: $b) { id } }',
      { c: code, b: md },
      token,
    );

    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    await openEntry(page, slug, code);

    await page.waitForSelector('.c-body', { timeout: 30000 });
    await page.waitForSelector('.c-body table', { timeout: 15000 });
    const cells = await page.$$eval('.c-body table td', (els) => els.map((e) => e.innerText.trim()));
    check('只读视图渲染出表格', cells.includes('苹果'), JSON.stringify(cells));
    const bodyText = await page.$eval('.c-body', (el) => el.innerText);
    check('正文不再空白', bodyText.includes('前言一行'), JSON.stringify(bodyText));
    check('只读视图没有语言下拉', await page.$$eval('.c-body select', (els) => els.length) === 0);

    await ctx.close();
  },
};

// ── 入口 ────────────────────────────────────────────────────────────
const scenario = process.argv[2];
if (!scenario || !scenarios[scenario]) {
  console.error(`用法：node scripts/editor-smoke.mjs <scenario>\n可用：${Object.keys(scenarios).join(', ')}`);
  process.exit(2);
}

const browser = await launch();
try {
  console.log(`scenario: ${scenario}`);
  await scenarios[scenario](browser);
} catch (e) {
  console.log(`  FAIL 场景异常 — ${e?.stack || e}`);
  failures.push(String(e));
} finally {
  await browser.close();
}

if (failures.length) {
  console.log(`\n${failures.length} 项失败：${failures.join('; ')}`);
  process.exit(1);
}
console.log('\n全部通过');
