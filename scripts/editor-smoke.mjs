// 编辑器浏览器验证脚本（playwright-core）。
//
// 用法：node scripts/editor-smoke.mjs <scenario>
//
// 需要先起一个 smoke server（默认 http://127.0.0.1:3099）。
// 环境变量：RODEO_BASE / RODEO_SITE_ROOT（默认 target/site-smoke）
//
// 踩过的坑：
//   - playwright-core 是 CommonJS，NODE_PATH 与 ESM 具名 import 都不 work，
//     只能按绝对路径 default import。
//   - debug wasm 会被 CompressionLayer 卡住（gzip 下 >60s），用 page.route
//     从磁盘 fulfill **/pkg/*.wasm。
//   - hash-files 会留好几代 hashed 文件，按 target/debug/hash.txt 精确匹配。
//   - localStorage.rodeo_jwt 是 JSON 编码过的字符串（带引号）。
//   - 不要预置 localStorage.rodeo_theme：SSR 不读它，预置会 hydration 报错。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.RODEO_BASE || 'http://127.0.0.1:3099';
const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const SITE_ROOT = process.env.RODEO_SITE_ROOT || 'target/site-smoke';
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

async function launch() {
  if (!fs.existsSync(EXECUTABLE)) throw new Error(`找不到 Chromium：${EXECUTABLE}`);
  return pw.chromium.launch({ executablePath: EXECUTABLE, headless: true });
}

async function newContext(browser, token) {
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  const wasm = fs.readFileSync(publishedWasm());
  await ctx.route('**/pkg/*.wasm', (r) =>
    r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }));
  if (token) {
    await ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
  }
  return ctx;
}

async function login(base = BASE) {
  const res = await fetch(`${base}/api/graphql`, {
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

async function gql(query, variables, token, base = BASE) {
  const res = await fetch(`${base}/api/graphql`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
    body: JSON.stringify({ query, variables }),
  });
  const json = await res.json();
  if (json.errors) throw new Error(`GraphQL 报错：${JSON.stringify(json.errors)}`);
  return json.data;
}

async function seedEntry(token, detail) {
  const ws = await gql('mutation($n: String!) { createWorkspace(name: $n) { id slug } }',
    { n: `smoke-${Date.now()}` }, token);
  const e = await gql(
    'mutation($id: ID!, $t: String!, $d: String) { createEntry(workspaceId: $id, title: $t, detail: $d) { code } }',
    { id: ws.createWorkspace.id, t: '编辑器冒烟条目', d: detail ?? '' }, token);
  return { workspaceId: ws.createWorkspace.id, slug: ws.createWorkspace.slug, code: e.createEntry.code };
}

async function openEntry(page, slug, code) {
  await page.goto(`${BASE}/${slug}/entry/${code}`, { waitUntil: 'domcontentloaded', timeout: 120000 });
  await page.waitForSelector('.tiptap-content', { timeout: 60000 });
  await page.waitForFunction(() => {
    const el = document.querySelector('.tiptap-content');
    return el && el.getAttribute('contenteditable') === 'true';
  }, { timeout: 30000 });
}

async function clickCmd(page, cmd) {
  const btn = await page.$(`.tiptap-toolbar .tiptap-tb-btn[data-cmd="${cmd}"]`);
  if (!btn) throw new Error(`工具栏上没有 ${cmd} 按钮`);
  await btn.click();
  await page.waitForTimeout(250);
}

const scenarios = {
  async 'tiptap-loaded'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);
    const st = await page.evaluate(() => ({
      bridge: typeof window.__rodeo_editor__,
      toolbar: document.querySelectorAll('.tiptap-toolbar').length,
      editable: document.querySelector('.tiptap-content')?.getAttribute('contenteditable'),
      cmds: [...document.querySelectorAll('.tiptap-tb-btn')].map((b) => b.dataset.cmd),
    }));
    eq('桥接已加载', st.bridge, 'object');
    // 条目详情页有两个编辑器（正文 + 评论输入框），各有自己的工具栏
    check('工具栏在', st.toolbar >= 1, `count=${st.toolbar}`);
    eq('编辑器可编辑', st.editable, 'true');
    check('工具栏命令齐全',
      ['bold', 'italic', 'h1', 'bulletList', 'codeBlock', 'table', 'undo', 'redo'].every((c) => st.cmds.includes(c)),
      st.cmds.join(','));
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  async 'syntax-basic'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);
    await clickCmd(page, 'codeBlock');
    await page.waitForSelector('.tiptap-content pre', { timeout: 15000 });
    // 光标进代码块，语言下拉才浮出来（它不是工具栏上的固定控件）
    await page.click('.tiptap-content pre');
    await page.waitForSelector('.tiptap-cb-lang:not([hidden])', { timeout: 10000 }).catch(() => null);
    const shown = await page.$eval('.tiptap-cb-lang', (el) => !el.hidden).catch(() => false);
    check('代码块上浮出语言下拉', shown);
    const langs = await page.$$eval('.tiptap-cb-lang option', (els) => els.map((e) => e.value));
    check('下拉有语言项', langs.length > 0, String(langs.length));
    check('下拉含 rust', langs.includes('rust'), langs.join(','));
    check('下拉含 javascript', langs.includes('javascript'), langs.join(','));
    await page.keyboard.type('fn main() {}');
    await page.selectOption('.tiptap-cb-lang', 'rust');
    await page.waitForTimeout(800);
    const cls = await page.$eval('.tiptap-content pre code', (el) => el.className).catch(() => '');
    check('代码块带上语言', String(cls).includes('rust'), String(cls));
    const tokens = await page.$$eval('.tiptap-content pre code span', (els) => els.length);
    check('高亮 token 出现', tokens >= 1, `count=${tokens}`);
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  async 'codeblock-enhance'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    // 代码块里的文本（第一个编辑器 = 条目正文）
    const blockText = async () =>
      page.$eval('.tiptap-content pre code', (el) => el.textContent);
    // 重置正文内容并把光标放到指定位置
    // （setContent 之后 Tiptap 的 TrailingNode 会补一个尾段落，
    //   focus('end') 会落到那个空段落里而不是代码块，所以这里显式给位置）
    const reset = async (html, pos) => {
      await page.evaluate(([h, p]) => {
        const ed = document.querySelector('.tiptap-content').closest('.tiptap-editor').__rodeo_editor;
        ed.commands.setContent(h);
        if (p != null) ed.commands.setTextSelection(p);
        ed.view.focus();
      }, [html, pos]);
      await page.waitForTimeout(250);
    };

    await clickCmd(page, 'codeBlock');
    await page.waitForSelector('.tiptap-content pre', { timeout: 15000 });

    // ① 括号自动补充：打 ( 补成 ()
    await page.keyboard.type('(');
    await page.waitForTimeout(300);
    eq('打 ( 自动补成 ()', await blockText(), '()');
    // 再打 ) ：光标右边已经有了，跳过不重复补
    await page.keyboard.type(')');
    await page.waitForTimeout(300);
    eq('再打 ) 不重复补', await blockText(), '()');

    // ② 选区被括号包起来：选中 abc 的 a 再打 [
    await reset('<pre><code>abc</code></pre>', 1);
    await page.keyboard.press('Shift+ArrowRight');
    await page.waitForTimeout(300); // 等 ProseMirror 把 DOM 选区同步进 state
    await page.keyboard.type('[');
    await page.waitForTimeout(300);
    eq('选区被括号包起来', await blockText(), '[a]bc');

    // ③ Enter 自动缩进：保持上一行的缩进
    await reset('<pre><code></code></pre>', 1);
    await page.keyboard.type('    let x = 1;');
    await page.keyboard.press('Enter');
    await page.keyboard.type('let y = 2;');
    await page.waitForTimeout(300);
    eq('Enter 保持缩进', await blockText(), '    let x = 1;\n    let y = 2;');

    // ④ 行尾是开括号时多缩进一级，闭括号挪到下一行
    await reset('<pre><code></code></pre>', 1);
    await page.keyboard.type('(');
    await page.keyboard.press('Enter');
    await page.waitForTimeout(300);
    eq('开括号回车展开成三行', await blockText(), '(\n    \n)');

    // ⑤ Tab = 4 个空格（不是制表符）
    await reset('<pre><code></code></pre>', 1);
    await page.keyboard.press('Tab');
    await page.waitForTimeout(300);
    eq('Tab 插 4 个空格', await blockText(), '    ');
    check('Tab 不是制表符', !(await blockText()).includes('\t'));

    // ⑥ 有选区时 Tab 给选区覆盖的每一行加缩进
    await reset('<pre><code>aaa\nbbb</code></pre>', 1);
    await page.evaluate(() => {
      const ed = document.querySelector('.tiptap-content').closest('.tiptap-editor').__rodeo_editor;
      ed.chain().setTextSelection({ from: 1, to: 5 }).focus().run();
    });
    await page.waitForTimeout(200);
    await page.keyboard.press('Tab');
    await page.waitForTimeout(300);
    eq('选区 Tab 两行都加缩进', await blockText(), '    aaa\n    bbb');

    // ⑦ 回归：正文（不在代码块）里 Tab 仍然插制表符
    await reset('<p>正文</p>', 1);
    await page.keyboard.press('Tab');
    await page.waitForTimeout(300);
    const bodyText = await page.$eval('.tiptap-content p', (el) => el.textContent);
    check('正文 Tab 仍插制表符', bodyText.includes('\t'), JSON.stringify(bodyText));

    // ⑧ 存进去再刷新，代码块内容（含缩进）还在
    await reset('<pre><code>    let x = 1;\n    let y = 2;</code></pre>', 1);
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 15000 });
    await save.click();
    await page.waitForTimeout(1500);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.tiptap-content pre code', { timeout: 60000 });
    eq('刷新后代码块内容还在', await blockText(), '    let x = 1;\n    let y = 2;');

    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  async 'syntax-persist'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    await openEntry(page, slug, code);
    await page.click('.tiptap-content');
    await page.keyboard.type('持久化测试 abc');
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 15000 });
    await save.click();
    await page.waitForTimeout(1500);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.tiptap-content', { timeout: 60000 });
    await page.waitForFunction(
      () => (document.querySelector('.tiptap-content')?.innerText || '').includes('持久化测试'),
      { timeout: 30000 });
    check('刷新后正文仍在', true);
    await ctx.close();
  },

  async 'table-basic'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);
    await clickCmd(page, 'table');
    await page.waitForSelector('.tiptap-content table', { timeout: 15000 });
    // 第一行是表头（<th>），所以 3 行共 9 个单元格要 td + th 一起数
    const tdCount = await page.$$eval('.tiptap-content table td, .tiptap-content table th', (els) => els.length);
    eq('插出 3×3 表格', tdCount, 9);
    const firstTd = await page.$('.tiptap-content table td');
    await firstTd.click();
    await page.keyboard.type('甲');
    const cellText = await page.$eval('.tiptap-content table td', (el) => el.innerText.trim());
    eq('单元格能打字', cellText, '甲');
    await page.waitForTimeout(500);
    const barVisible = await page.$eval('.tiptap-tablebar', (el) => !el.hidden).catch(() => false);
    check('表格浮动条出现', barVisible);
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 15000 });
    await save.click();
    await page.waitForTimeout(1500);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.tiptap-content', { timeout: 60000 });
    await page.waitForSelector('.tiptap-content table', { timeout: 30000 });
    const afterTd = await page.$$eval('.tiptap-content table td, .tiptap-content table th', (els) => els.length);
    eq('刷新后表格还在', afterTd, 9);
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  async 'table-extra'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);
    await clickCmd(page, 'table');
    await page.waitForSelector('.tiptap-content table', { timeout: 15000 });
    const firstTd = await page.$('.tiptap-content table td');
    await firstTd.click();
    await page.keyboard.type('甲');
    await page.keyboard.press('Shift+Enter');
    await page.keyboard.type('乙');
    const cellText = await page.$eval('.tiptap-content table td', (el) => el.innerText);
    check('单元格内可换行', cellText.includes('甲') && cellText.includes('乙'), JSON.stringify(cellText));
    const tablesBefore = await page.$$eval('.tiptap-content table', (els) => els.length);
    await page.evaluate(() => {
      const dt = new DataTransfer();
      dt.setData('text/plain', '粘进来的字');
      document.querySelector('.tiptap-content table td').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    });
    await page.waitForTimeout(800);
    const tablesAfter = await page.$$eval('.tiptap-content table', (els) => els.length);
    eq('往单元格里粘贴不插新表', tablesAfter, tablesBefore);
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

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
      document.querySelector('.tiptap-content').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    }, md);
    await page.waitForSelector('.tiptap-content table', { timeout: 15000 });
    const cells = await page.$$eval('.tiptap-content table td, .tiptap-content table th',
      (els) => els.map((e) => e.innerText.trim()));
    check('表格有 6 个单元格', cells.length === 6, JSON.stringify(cells));
    check('单元格内容是苹果', cells.includes('苹果'), JSON.stringify(cells));
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

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
      document.querySelector('.tiptap-content').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    });
    await page.waitForSelector('.tiptap-content table', { timeout: 15000 });
    const strong = await page.$$eval('.tiptap-content strong', (els) => els.map((e) => e.innerText.trim()));
    check('正文仍是粗体', strong.includes('粗体正文'), JSON.stringify(strong));
    const cells = await page.$$eval('.tiptap-content table td, .tiptap-content table th',
      (els) => els.map((e) => e.innerText.trim()));
    check('表格是真表格', cells.includes('1') && cells.includes('2'), JSON.stringify(cells));
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  async 'md-table-readonly'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const md = '前言一行\n\n| 名称 | 数量 |\n| --- | --- |\n| 苹果 | 3 |';
    await gql('mutation($c: String!, $b: String!) { createComment(entryCode: $c, body: $b) { id } }',
      { c: code, b: md }, token);
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    await openEntry(page, slug, code);
    await page.waitForSelector('.c-body', { timeout: 30000 });
    await page.waitForSelector('.c-body table', { timeout: 15000 });
    const cells = await page.$$eval('.c-body table td, .c-body table th', (els) => els.map((e) => e.innerText.trim()));
    check('只读视图渲染出表格', cells.includes('苹果'), JSON.stringify(cells));
    const bodyText = await page.$eval('.c-body', (el) => el.innerText);
    check('正文不再空白', bodyText.includes('前言一行'), JSON.stringify(bodyText));
    await ctx.close();
  },

  async 'editor-regression'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);
    await clickCmd(page, 'taskList');
    await page.waitForTimeout(400);
    check('任务列表能插入',
      await page.$$eval('.tiptap-content ul[data-type="taskList"] li', (els) => els.length) >= 1);
    await page.click('.tiptap-content');
    await page.keyboard.type('@');
    await page.waitForTimeout(1200);
    // 光看「有没有 .tiptap-mention 元素」不够：候选表为空时浮层照样建出来，
    // 只是 hidden —— 2026-11-02 的 @提及失效就是这么漏过去的。必须查候选项。
    const mention = await page.evaluate(() => {
      const dd = document.querySelector('.tiptap-mention');
      return {
        exists: !!dd,
        hidden: dd ? dd.hidden : null,
        items: dd ? [...dd.querySelectorAll('.tiptap-mention-item')].map((e) => e.textContent) : [],
      };
    });
    check('@提及浮层在', mention.exists);
    check('@提及浮层可见', mention.hidden === false, JSON.stringify(mention));
    check('@提及有候选', mention.items.length >= 1, JSON.stringify(mention.items));
    await clickCmd(page, 'h2');
    await page.waitForTimeout(400);
    check('标题能插入', await page.$$eval('.tiptap-content h2', (els) => els.length) >= 1);
    check('链接按钮存在', !!(await page.$('.tiptap-toolbar .tiptap-tb-btn[data-cmd="link"]')));
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  // @提及的完整链路：打 @ → 出候选 → 选中 → 正文里落成 mention chip →
  // 存盘 → 刷新后 chip 还在（服务端消毒放行 data-*，见 service/richtext.rs）。
  async 'mention-dropdown'(browser) {
    const token = await login();
    const { slug, code, workspaceId } = await seedEntry(token, '');
    const members = await gql('query($id: ID!) { members(workspaceId: $id) { accountId name } }',
      { id: workspaceId }, token);
    check('工作空间有成员可提', members.members.length >= 1, JSON.stringify(members.members));
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);

    await page.click('.tiptap-content');
    await page.keyboard.type('找 ');
    await page.keyboard.type('@');
    await page.waitForTimeout(1200);
    const dd = await page.evaluate(() => {
      const el = document.querySelector('.tiptap-mention');
      return {
        hidden: el ? el.hidden : null,
        items: el ? [...el.querySelectorAll('.tiptap-mention-item')].map((e) => e.textContent) : [],
      };
    });
    check('@ 浮层可见', dd.hidden === false, JSON.stringify(dd));
    check('@ 浮层列出成员', dd.items.length >= 1, JSON.stringify(dd.items));
    const expect = members.members[0].name;
    check('@ 浮层里是工作空间成员', dd.items.includes(expect), JSON.stringify(dd.items));

    // Enter 选中第一项 → 正文里落成 mention chip
    await page.keyboard.press('Enter');
    await page.waitForTimeout(600);
    const chip = await page.evaluate(() => {
      const el = document.querySelector('.tiptap-content .tiptap-mention-chip');
      return el ? { text: el.textContent, id: el.getAttribute('data-id'), label: el.getAttribute('data-label') } : null;
    });
    check('正文里落成 mention chip', !!chip, JSON.stringify(chip));
    check('chip 显示 @姓名', chip && chip.text === `@${expect}`, JSON.stringify(chip));
    check('chip 带 data-id', chip && chip.id === members.members[0].accountId, JSON.stringify(chip));

    // 存盘 → 刷新：chip 还在（服务端消毒放行 data-*）
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 20000 });
    await save.click();
    await page.waitForTimeout(2000);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.tiptap-content', { timeout: 60000 });
    await page.waitForSelector('.tiptap-content [data-type="mention"]', { timeout: 30000 });
    const after = await page.$eval('.tiptap-content [data-type="mention"]', (el) => el.textContent);
    eq('刷新后 mention 还在', after, `@${expect}`);
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },

  async 'image-paste'(browser) {
    const token = await login();
    const { slug, code } = await seedEntry(token, '');
    const ctx = await newContext(browser, token);
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(String(e)));
    await openEntry(page, slug, code);
    const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';
    await page.evaluate(async (b64) => {
      const bin = atob(b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      const file = new File([bytes], 'shot.png', { type: 'image/png' });
      const dt = new DataTransfer();
      dt.items.add(file);
      document.querySelector('.tiptap-content').dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    }, png);
    await page.waitForSelector('.tiptap-content img', { timeout: 30000 });
    const src = await page.$eval('.tiptap-content img', (el) => el.getAttribute('src'));
    check('图片插进编辑器', !!src && src.includes('/api/attachments/'), String(src));
    const save = await page.waitForSelector('.entry-top button.btn.pri:not([disabled])', { timeout: 20000 });
    await save.click();
    await page.waitForTimeout(2000);
    const errText = await page.$eval('.entry-top', (el) => el.innerText).catch(() => '');
    check('上传后保存不报冲突', !errText.includes('冲突'), JSON.stringify(errText.slice(0, 200)));
    check('页面无 JS 错误', errors.length === 0, errors.join(' | '));
    await ctx.close();
  },
};

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
