// 视图导航栏右键菜单的回归 E2E：3 个不同视图 × 20 轮配置视图点击，
// 每个 dialog 必须在 t100 / t500 / t1500ms 三档都能看到。
//
// 历史缺陷（spec/20261015-待回归功能.md 第 4 条）：
//   "视图配置右键菜单经常无法点击问题。视图配置对话框点击后消失的问题"
// 根因（src/frontend/pages/workspace_main.rs:683-718 commit 9f6541a）：
//   Leptos 0.8 window 级事件委托 + `.vmenu` 上的 stop_propagation 拦不住
//   同 element 的 sibling listener，导致 mousedown 抢先关掉 vmenu，
//   click 还没跑按钮就被拆掉（点无反应）或 dmodal 背景接住 click 后
//   立刻关掉刚开的弹窗（闪现后消失）。`composed_path()` 自检 .vmenu
//   修复之后，本脚本做回归。
//
// 跑法（参考 scripts/agent-e2e.mjs 的 setup）：
//   1. 起 smoke server：./target/debug/rodeo /tmp/smoke.toml
//      （环境：LEPTOS_SITE_ADDR=127.0.0.1:3099
//            LEPTOS_SITE_ROOT=target/site
//            LEPTOS_HASH_FILES=true
//            LEPTOS_OUTPUT_NAME=rodeo）
//   2. node scripts/view-config-menu-e2e.mjs
// 任何 case 失败不中断；汇总后用 process.exit(1)。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'fs/promises';

const SERVER = process.env.RODEO_SERVER || 'http://127.0.0.1:3099';
const EMAIL = process.env.RODEO_EMAIL || 'admin@local';
const PASSWORD = process.env.RODEO_PASS || 'Admin12345';
const CHROME =
  '/Users/wangxiaoyan/Library/Caches/ms-playwright/chromium-1217/' +
  'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing';
const VIEW_COUNT = 3;
const CYCLES = 20;

// ---------- helpers ----------

function nowIso() { return new Date().toISOString(); }
function fail(name, msg, extra) { return { name, pass: false, fail: msg, extra }; }
function pass(name, ms) { return { name, pass: true, ms }; }

async function gql(token, query, variables = {}) {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      authorization: 'Bearer ' + token,
    },
    body: JSON.stringify({ query, variables }),
  });
  const j = await r.json();
  if (j.errors && j.errors.length) {
    throw new Error('GraphQL: ' + (j.errors[0].message || JSON.stringify(j.errors)));
  }
  return j.data;
}

async function loginToken() {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      query: `mutation { login(email: "${EMAIL}", password: "${PASSWORD}") { token } }`,
    }),
  });
  const j = await r.json();
  if (!j.data || !j.data.login || !j.data.login.token) {
    throw new Error('login failed: ' + JSON.stringify(j));
  }
  return j.data.login.token;
}

// rodeo_jwt 必须 JSON-encode 后写进 localStorage——gloo_storage 用 serde_json
// 反序列化，存 raw 字符串会被反序列化失败当成 None。
function injectTokenCtx(ctx, token) {
  return ctx.addInitScript((t) => {
    localStorage.setItem('rodeo_jwt', JSON.stringify(t));
  }, token);
}

// bootstrap：建一个工作空间 + N 个视图，返回 slug 与 view ids。
async function bootstrap(token) {
  const slug = 'rc' + Date.now().toString(36).slice(-8);
  const ws = await gql(token,
    'mutation($n:String!,$s:String!){ createWorkspace(name:$n,slug:$s){id slug} }',
    { n: '视图配置 E2E', s: slug });
  const wsId = ws.createWorkspace.id;
  const ids = [];
  for (let i = 0; i < VIEW_COUNT; i++) {
    const v = await gql(token,
      'mutation($w:ID!,$n:String!,$q:JSON!){ createView(workspaceId:$w,name:$n,query:$q,columns:[],isShared:true){id name} }',
      { w: wsId, n: `view-${i+1}`, q: { and: [] } });
    ids.push(v.createView.id);
  }
  return { slug, wsId, viewIds: ids };
}

// 探测 mousedown 路径里有没有 .vmenu——这是 9f6541a 修复的关键点。
async function probeComposedPath(page) {
  return await page.evaluate(() => {
    const log = window.__vmenuMousedownLogs || [];
    return log.slice(-3).map(l => ({
      target: l.target,
      path: l.path.join(' > '),
    }));
  });
}

// ---------- 主 case ----------

const CASES = [];

CASES.push(async ({ browser, token, slug }) => {
  const t = Date.now();
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  page.setDefaultTimeout(15000);

  // 拦截第一次 mousedown 记录 composed_path
  await page.addInitScript(() => {
    window.__vmenuMousedownLogs = [];
    document.addEventListener('mousedown', (ev) => {
      const path = ev.composedPath().map(n => {
        if (n.nodeType === 1) return `${n.tagName}.${n.className || '(no class)'}`;
        if (n.nodeType === 3) return `#text("${n.textContent.slice(0, 20)}")`;
        if (n.nodeType === 9) return '#document';
        return `#nodeType${n.nodeType}`;
      });
      window.__vmenuMousedownLogs.push({ target: ev.target.tagName || ev.target.nodeName, path });
    }, true);
  });

  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 15000 });
  } catch (e) {
    // goto 在长轮询 / 慢 hydration 时偶尔超时，commit 之后页面已经在渲染，跳过超时
  }
  // 等 hydration 推完 —— workspace data load 之后再发 views() 请求，
  // 整个链 ~6s；保险起见等 15s 后再 waitForFunction，避免在 queries 排队前就误判失败。
  await page.waitForTimeout(15000);
  // 等视图全量 hydration 进侧栏：3 个 view 行（每个 view-* 都有 padding-left 内联样式）
  await page.waitForFunction(
    (n) => Array.from(document.querySelectorAll('.wside .it')).filter(el => {
      const s = el.getAttribute('style');
      return s && s.includes('padding-left');
    }).length >= n,
    VIEW_COUNT,
    { timeout: 60000, polling: 200 },
  ).catch(async (e) => {
    const debug = await page.evaluate(() => {
      const aside = document.querySelector('.wside');
      const items = aside ? Array.from(aside.querySelectorAll('.it')).map(el => ({
        cls: el.className, style: el.getAttribute('style'), text: (el.innerText || '').slice(0, 40),
      })) : [];
      return { hasAside: !!aside, items, bodyStart: document.body.innerText.slice(0, 300) };
    });
    console.error('[debug]', JSON.stringify(debug, null, 2).slice(0, 1500));
    throw e;
  });

  // 清空日志
  await page.evaluate(() => { window.__vmenuMousedownLogs = []; });

  const results = [];
  for (let cyc = 0; cyc < CYCLES; cyc++) {
    for (let vIdx = 0; vIdx < VIEW_COUNT; vIdx++) {
      const viewName = `view-${vIdx+1}`;
      const rowBox = await page.evaluate((name) => {
        const rows = Array.from(document.querySelectorAll('.wside .it'));
        for (const r of rows) {
          if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes(name)) {
            const b = r.getBoundingClientRect();
            return { x: b.x, y: b.y, w: b.width, h: b.height };
          }
        }
        return null;
      }, viewName);
      if (!rowBox) {
        results.push({ cyc, vIdx, viewName, ok: false, why: 'view-row-not-found' });
        continue;
      }
      // 右键弹菜单
      await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2),
                             Math.round(rowBox.y + rowBox.h / 2),
                             { button: 'right' });
      await page.waitForTimeout(150);
      const menuState = await page.evaluate(() => {
        const ms = document.querySelectorAll('.vmenu');
        return { count: ms.length };
      });
      if (menuState.count === 0) {
        results.push({ cyc, vIdx, viewName, ok: false, why: 'no-vmenu' });
        continue;
      }
      // 点 配置视图 —— 用 mouse.down + 停顿 + mouse.up 模拟真实人类
      const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
      if (!cfgBtn) {
        results.push({ cyc, vIdx, viewName, ok: false, why: 'no-cfg-button' });
        await page.keyboard.press('Escape').catch(() => {});
        continue;
      }
      const cb = await cfgBtn.boundingBox();
      await page.mouse.move(Math.round(cb.x + cb.width / 2), Math.round(cb.y + cb.height / 2));
      await page.mouse.down();
      await page.waitForTimeout(50);
      await page.mouse.up();
      // 三档时间点都看 dialog：这是「对话框闪现后消失」症状的探针。
      const t100 = await page.evaluate(() => ({ dmodal: !!document.querySelector('.dmodal') }));
      await page.waitForTimeout(400);
      const t500 = await page.evaluate(() => ({ dmodal: !!document.querySelector('.dmodal') }));
      await page.waitForTimeout(1000);
      const t1500 = await page.evaluate(() => ({ dmodal: !!document.querySelector('.dmodal') }));

      const ok = t100.dmodal && t500.dmodal && t1500.dmodal;
      const why = !t100.dmodal ? 'no-dialog-at-100ms'
        : !t500.dmodal ? 'dialog-closed-by-500ms'
        : !t1500.dmodal ? 'dialog-closed-by-1500ms' : '';
      results.push({ cyc, vIdx, viewName, ok, why });

      // 关弹窗为下一轮准备
      const cancelBtn = await page.$('.dmodal .dmbox button:has-text("取消")');
      if (cancelBtn) {
        await cancelBtn.click();
      } else {
        await page.mouse.click(10, 10);
      }
      await page.waitForTimeout(150);
    }
  }

  const probe = await probeComposedPath(page);

  const total = results.length;
  const passCnt = results.filter(r => r.ok).length;
  const failed = results.filter(r => !r.ok);
  if (failed.length === 0) {
    return pass(`配置视图 E2E ${passCnt}/${total}`, Date.now() - t);
  }
  return fail(`配置视图 E2E`, `${passCnt}/${total} 通过`, { failed, probe });
});

// ---------- Main ----------

(async () => {
  console.log('[' + nowIso() + '] launching browser');
  const browser = await pw.chromium.launch({
    executablePath: CHROME,
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage'],
  });

  let token;
  try {
    token = await loginToken();
    console.log('[' + nowIso() + '] login OK, token len=' + token.length);
  } catch (e) {
    console.error('login failed: ' + e.message);
    process.exit(2);
  }

  let bs;
  try {
    bs = await bootstrap(token);
    console.log(`[${nowIso()}] bootstrap ws=${bs.slug} views=${bs.viewIds.length}`);
  } catch (e) {
    console.error('bootstrap failed: ' + e.message);
    process.exit(2);
  }

  const results = [];
  for (const c of CASES) {
    const r = await c({ browser, token, slug: bs.slug });
    results.push(r);
    const tag = r.pass ? 'PASS' : 'FAIL';
    const extra = r.pass ? `${r.ms}ms` : r.fail + (r.extra ? ' ' + JSON.stringify(r.extra) : '');
    console.log('[' + nowIso() + '] ' + tag + ' ' + r.name + ' — ' + extra);
  }

  await browser.close();

  const failed = results.filter(r => !r.pass);
  console.log('');
  console.log('=== 汇总 ===');
  console.log(`total=${results.length} pass=${results.length - failed.length} fail=${failed.length}`);
  if (failed.length) {
    for (const f of failed) console.log('  - ' + f.name + ': ' + f.fail);
    process.exit(1);
  }
})();