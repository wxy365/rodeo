// 视图描述的端到端验证：
//   1. 配置视图弹窗里写描述 → 保存 → 落库
//   2. 描述写进「被配置的那个视图」，不是当前激活的视图（右键不切换激活视图）
//   3. 激活该视图后，标题下方出现描述行；侧栏 tooltip 也带描述
//   4. 刷新后仍在；再开弹窗 textarea 回填；清空后消失
//
// 跑法（参考 scripts/view-config-menu-e2e.mjs）：
//   RODEO_SERVER=https://127.0.0.1:3000 \
//   NODE_TLS_REJECT_UNAUTHORIZED=0 node scripts/view-description-e2e.mjs
// 任何 case 失败不中断；汇总后用 process.exit(1)。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';
import fs from 'fs/promises';

const SERVER = process.env.RODEO_SERVER || 'http://127.0.0.1:3099';
const EMAIL = process.env.RODEO_EMAIL || 'admin@local';
const PASSWORD = process.env.RODEO_PASS || 'Admin12345';
const CHROME =
  '/Users/wangxiaoyan/Library/Caches/ms-playwright/chromium-1217/' +
  'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing';

const TARGET = 'desc-target';   // 被配置描述的视图
const OTHER = 'desc-other';     // 保持激活、用来验证「没写错视图」
const DESC = '这个视图只看本周要交付的条目';

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
// 反序列化，存 raw 字符串会被当成 None。
function injectTokenCtx(ctx, token) {
  return ctx.addInitScript((t) => {
    localStorage.setItem('rodeo_jwt', JSON.stringify(t));
  }, token);
}

async function bootstrap(token) {
  const slug = 'vd' + Date.now().toString(36).slice(-8);
  const ws = await gql(token,
    'mutation($n:String!,$s:String!){ createWorkspace(name:$n,slug:$s){id slug} }',
    { n: '视图描述 E2E', s: slug });
  const wsId = ws.createWorkspace.id;
  const ids = {};
  for (const name of [TARGET, OTHER]) {
    const v = await gql(token,
      'mutation($w:ID!,$n:String!,$q:JSON!){ createView(workspaceId:$w,name:$n,query:$q,columns:[],isShared:true){id name} }',
      { w: wsId, n: name, q: { and: [] } });
    ids[name] = v.createView.id;
  }
  return { slug, wsId, ids };
}

async function cleanup(token, wsId) {
  try {
    await gql(token, 'mutation($w:ID!){ deleteWorkspace(workspaceId:$w) }', { w: wsId });
  } catch (e) {
    console.error('cleanup failed: ' + e.message);
  }
}

// 打开工作空间页面并等侧栏视图行就位。
//
// debug wasm 有 38MB，走 CompressionLayer 时 Chromium 的请求会一直挂着
// （hydration 等不到 wasm，页面停在「加载中…」）。改成从磁盘直接 fulfill。
async function openWorkspace(browser, token, slug) {
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true });
  injectTokenCtx(ctx, token);
  await ctx.route('**/pkg/*.wasm', async (route) => {
    const url = route.request().url();
    const name = url.slice(url.lastIndexOf('/') + 1);
    const body = await fs.readFile('target/site/pkg/' + name);
    await route.fulfill({ status: 200, contentType: 'application/wasm', body });
  });
  const page = await ctx.newPage();
  page.setDefaultTimeout(20000);
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 20000 });
  } catch (e) {
    // goto 在慢 hydration 时偶尔超时，commit 之后页面已经在渲染。
  }
  await page.waitForTimeout(15000);
  await page.waitForFunction(
    (n) => Array.from(document.querySelectorAll('.wside .it'))
      .filter(el => (el.getAttribute('style') || '').includes('padding-left'))
      .length >= n,
    2,
    { timeout: 60000, polling: 200 },
  );
  return { ctx, page };
}

// 侧栏里某个视图行的坐标。
async function viewRowBox(page, viewName) {
  return await page.evaluate((name) => {
    for (const r of document.querySelectorAll('.wside .it')) {
      if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes(name)) {
        const b = r.getBoundingClientRect();
        return { x: b.x, y: b.y, w: b.width, h: b.height };
      }
    }
    return null;
  }, viewName);
}

// 点侧栏视图行（切换激活视图）。
async function clickView(page, viewName) {
  const box = await viewRowBox(page, viewName);
  if (!box) throw new Error('view row not found: ' + viewName);
  await page.mouse.click(Math.round(box.x + box.w / 2), Math.round(box.y + box.h / 2));
  await page.waitForTimeout(1500);
}

// 右键点侧栏视图行 → 点「配置视图」→ 返回弹窗里的描述 textarea。
async function openConfigDialog(page, viewName) {
  const box = await viewRowBox(page, viewName);
  if (!box) throw new Error('view row not found: ' + viewName);
  await page.mouse.click(Math.round(box.x + box.w / 2), Math.round(box.y + box.h / 2),
    { button: 'right' });
  await page.waitForTimeout(250);
  const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
  if (!cfgBtn) throw new Error('no 配置视图 button in vmenu');
  const cb = await cfgBtn.boundingBox();
  await page.mouse.move(Math.round(cb.x + cb.width / 2), Math.round(cb.y + cb.height / 2));
  await page.mouse.down();
  await page.waitForTimeout(50);
  await page.mouse.up();
  await page.waitForSelector('.dmodal textarea.inp', { timeout: 15000 });
  return page.$('.dmodal textarea.inp');
}

// 点弹窗里的「保存」。Leptos 的 disabled 是异步更新的——先等按钮可点。
async function clickSave(page) {
  await page.waitForFunction(() => {
    const b = Array.from(document.querySelectorAll('.dmodal button'))
      .find(x => x.textContent.trim() === '保存');
    return b && !b.disabled;
  }, undefined, { timeout: 15000, polling: 100 });
  await page.click('.dmodal button:has-text("保存")');
  await page.waitForSelector('.dmodal', { state: 'detached', timeout: 20000 });
}

async function descriptions(token, wsId) {
  const q = await gql(token, 'query($w:ID!){ views(workspaceId:$w){ id name description } }', { w: wsId });
  const out = {};
  for (const v of q.views || []) out[v.name] = v.description;
  return out;
}

// ---------- cases ----------

const CASES = [];

CASES.push(async ({ browser, token, slug, wsId, ids }) => {
  const t = Date.now();
  const { ctx, page } = await openWorkspace(browser, token, slug);
  try {
    // 让 OTHER 成为当前激活视图——随后配置 TARGET 时，激活的仍是 OTHER。
    await clickView(page, OTHER);

    const ta = await openConfigDialog(page, TARGET);
    await ta.click();
    await ta.fill(DESC);
    await clickSave(page);

    // 描述必须落在「被配置的那个视图」上，不能落到当前激活的 OTHER 上。
    const descs = await descriptions(token, wsId);
    if (descs[TARGET] !== DESC) {
      return fail('描述落到被配置的视图', `views[${TARGET}].description=${JSON.stringify(descs[TARGET])}`);
    }
    if ((descs[OTHER] || '') !== '') {
      return fail('描述没落到别的视图', `views[${OTHER}].description=${JSON.stringify(descs[OTHER])}`);
    }

    // 侧栏那一行的 tooltip 里带描述。
    const tip = await page.evaluate((name) => {
      for (const r of document.querySelectorAll('.wside .it')) {
        if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes(name)) {
          return r.getAttribute('title') || '';
        }
      }
      return null;
    }, TARGET);
    if (!tip || !tip.includes(DESC)) {
      return fail('侧栏 tooltip 带描述', `title=${JSON.stringify(tip)}`);
    }

    return pass('描述落到被配置的视图 / 侧栏 tooltip', Date.now() - t);
  } catch (e) {
    return fail('描述落到被配置的视图 / 侧栏 tooltip', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async ({ browser, token, slug, wsId, ids }) => {
  const t = Date.now();
  const { ctx, page } = await openWorkspace(browser, token, slug);
  try {
    // 激活 TARGET：标题下方应出现描述行。
    await clickView(page, TARGET);
    await page.waitForSelector('.vdesc', { timeout: 20000 });
    const shown = (await page.textContent('.vdesc') || '').trim();
    if (shown !== DESC) {
      return fail('激活后显示描述', `.vdesc=「${shown}」，期望「${DESC}」`);
    }

    // 再开弹窗，textarea 应回填已存的描述。
    const ta = await openConfigDialog(page, TARGET);
    const prefill = await ta.inputValue();
    if (prefill !== DESC) {
      return fail('弹窗回填', `textarea=${JSON.stringify(prefill)}`);
    }

    // 清空 → 保存 → 描述消失。
    await ta.fill('');
    await clickSave(page);
    await page.waitForTimeout(1000);
    const stillThere = await page.evaluate(() => !!document.querySelector('.vdesc'));
    if (stillThere) return fail('清空后消失', '.vdesc 还在');
    const descs = await descriptions(token, wsId);
    if ((descs[TARGET] || '') !== '') {
      return fail('清空后消失', `服务端 description=${JSON.stringify(descs[TARGET])}`);
    }
    return pass('激活后显示 / 弹窗回填 / 清空后消失', Date.now() - t);
  } catch (e) {
    return fail('激活后显示 / 弹窗回填 / 清空后消失', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async ({ browser, token, slug, wsId, ids }) => {
  const t = Date.now();
  const { ctx, page } = await openWorkspace(browser, token, slug);
  try {
    await clickView(page, TARGET);
    const ta = await openConfigDialog(page, TARGET);
    await ta.click();
    await ta.fill(DESC);
    await clickSave(page);
    await page.waitForTimeout(500);

    // 刷新（同一个 context 重新 goto）后描述仍在——说明是持久化，不是内存态。
    await page.reload({ waitUntil: 'commit', timeout: 20000 }).catch(() => {});
    await page.waitForTimeout(15000);
    await clickView(page, TARGET);
    await page.waitForSelector('.vdesc', { timeout: 30000 });
    const shown = (await page.textContent('.vdesc') || '').trim();
    if (shown !== DESC) {
      return fail('刷新后仍在', `.vdesc=「${shown}」`);
    }
    const descs = await descriptions(token, wsId);
    if (descs[TARGET] !== DESC) {
      return fail('刷新后仍在', `服务端 description=${JSON.stringify(descs[TARGET])}`);
    }
    return pass('刷新后仍在', Date.now() - t);
  } catch (e) {
    return fail('刷新后仍在', e.message);
  } finally {
    await ctx.close();
  }
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
    console.log(`[${nowIso()}] bootstrap ws=${bs.slug} views=${JSON.stringify(bs.ids)}`);
  } catch (e) {
    console.error('bootstrap failed: ' + e.message);
    process.exit(2);
  }

  const results = [];
  for (const c of CASES) {
    const r = await c({ browser, token, ...bs });
    results.push(r);
    const tag = r.pass ? 'PASS' : 'FAIL';
    const extra = r.pass ? `${r.ms}ms` : r.fail + (r.extra ? ' ' + JSON.stringify(r.extra) : '');
    console.log('[' + nowIso() + '] ' + tag + ' ' + r.name + ' — ' + extra);
  }

  await browser.close();
  await cleanup(token, bs.wsId);

  const failed = results.filter(r => !r.pass);
  console.log('');
  console.log('=== 汇总 ===');
  console.log(`total=${results.length} pass=${results.length - failed.length} fail=${failed.length}`);
  if (failed.length) {
    for (const f of failed) console.log('  - ' + f.name + ': ' + f.fail);
    process.exit(1);
  }
})();
