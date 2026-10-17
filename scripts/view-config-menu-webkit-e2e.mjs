// WebKit 兜底路径验证：用 playwright-core 的 webkit 引擎（与 Safari 同源 WebKit）跑
// view-config-menu E2E，专门触发「composed_path 在 mousedown 上截断」这条 Safari quirk，
// 验证 dc3e968 的 target.parentNode 链兜底能让 vmenu 不被误关。
//
// 跑法同 view-config-menu-e2e.mjs（沿用同一套 smoke server）。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';

const SERVER = process.env.RODEO_SERVER || 'http://127.0.0.1:3099';
const EMAIL = process.env.RODEO_EMAIL || 'admin@local';
const PASSWORD = process.env.RODEO_PASS || 'Admin12345';
const VIEW_COUNT = 3;
const CYCLES = 20;

function nowIso() { return new Date().toISOString(); }
function fail(name, msg, extra) { return { name, pass: false, fail: msg, extra }; }
function pass(name, ms) { return { name, pass: true, ms }; }

async function gql(token, query, variables = {}) {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer ' + token },
    body: JSON.stringify({ query, variables }),
  });
  const j = await r.json();
  if (j.errors && j.errors.length) throw new Error('GraphQL: ' + (j.errors[0].message || JSON.stringify(j.errors)));
  return j.data;
}
async function loginToken() {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ query: `mutation { login(email: "${EMAIL}", password: "${PASSWORD}") { token } }` }),
  });
  const j = await r.json();
  if (!j.data || !j.data.login || !j.data.login.token) throw new Error('login failed: ' + JSON.stringify(j));
  return j.data.login.token;
}
function injectTokenCtx(ctx, token) {
  return ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
}
async function bootstrap(token) {
  const slug = 'rc' + Date.now().toString(36).slice(-8);
  const ws = await gql(token,
    'mutation($n:String!,$s:String!){ createWorkspace(name:$n,slug:$s){id slug} }',
    { n: 'WebKit E2E', s: slug });
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

(async () => {
  console.log('[' + nowIso() + '] launching webkit');
  // playwright-core 是 CJS：import pw from ... 拿到的是 { default: { chromium, webkit, ... } }
  // 动态 import() 时同样要 .default
  const pwCore = pw.default || pw;
  console.log('[probe] pwCore.chromium:', typeof pwCore.chromium, 'pwCore.webkit:', typeof pwCore.webkit);
  const browser = await pwCore.webkit.launch({
    headless: true,
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

  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  page.setDefaultTimeout(15000);
  // 网络监听：看 webkit 有没有发出 graphql 请求
  page.on('request', r => {
    const u = r.url();
    if (u.includes('/api/graphql') || u.includes('/pkg/')) {
      console.log('[net-req]', r.method(), u.split('/').pop());
    }
  });
  page.on('response', r => {
    const u = r.url();
    if (u.includes('/api/graphql') || u.includes('/pkg/')) {
      console.log('[net-resp]', r.status(), u.split('/').pop());
    }
  });
  page.on('pageerror', e => console.error('[pageerror]', e.message));
  page.on('console', m => { if (m.type() === 'error') console.error('[browser:error]', m.text().slice(0, 200)); });
  // 探针：composed_path 在 webkit 上返回什么
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
    await page.goto(`${SERVER}/${bs.slug}`, { waitUntil: 'commit', timeout: 15000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(15000);
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

  await page.evaluate(() => { window.__vmenuMousedownLogs = []; });

  const t0 = Date.now();
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
      await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2),
                             Math.round(rowBox.y + rowBox.h / 2),
                             { button: 'right' });
      await page.waitForTimeout(150);
      const menuState = await page.evaluate(() => ({ count: document.querySelectorAll('.vmenu').length }));
      if (menuState.count === 0) {
        results.push({ cyc, vIdx, viewName, ok: false, why: 'no-vmenu' });
        continue;
      }
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

      const cancelBtn = await page.$('.dmodal .dmbox button:has-text("取消")');
      if (cancelBtn) {
        await cancelBtn.click();
      } else {
        await page.mouse.click(10, 10);
      }
      await page.waitForTimeout(150);
    }
  }

  const probe = await page.evaluate(() => (window.__vmenuMousedownLogs || []).slice(-3));

  const total = results.length;
  const passCnt = results.filter(r => r.ok).length;
  const failed = results.filter(r => !r.ok);

  await browser.close();

  console.log('=== 汇总 ===');
  console.log(`total=${total} pass=${passCnt} fail=${failed.length} time=${Date.now() - t0}ms`);
  for (let v = 0; v < VIEW_COUNT; v++) {
    const vr = results.filter(x => x.vIdx === v);
    const vp = vr.filter(x => x.ok).length;
    console.log(`  view-${v+1}: ${vp}/${vr.length}`);
  }
  if (probe.length) {
    console.log('=== composed_path 探针 ===');
    for (const l of probe) {
      console.log(`  target=${l.target}`);
      console.log(`  path=${l.path.join(' > ')}`);
    }
  }
  if (failed.length) {
    for (const f of failed) console.log('  - ' + JSON.stringify(f));
    process.exit(1);
  }
})();