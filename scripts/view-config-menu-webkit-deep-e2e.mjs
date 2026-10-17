// 视图配置对话框深度验证（WebKit）：除了「能弹出来」还要「里面能填能保存」。
//
// 跑法同 view-config-menu-webkit-e2e.mjs（沿用同一套 smoke server）。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';

const SERVER = process.env.RODEO_SERVER || 'http://127.0.0.1:3099';
const EMAIL = process.env.RODEO_EMAIL || 'admin@local';
const PASSWORD = process.env.RODEO_PASS || 'Admin12345';

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
async function bootstrap(token) {
  const slug = 'rc' + Date.now().toString(36).slice(-8);
  const ws = await gql(token,
    'mutation($n:String!,$s:String!){ createWorkspace(name:$n,slug:$s){id slug} }',
    { n: 'WebKit Deep E2E', s: slug });
  const wsId = ws.createWorkspace.id;
  const v = await gql(token,
    'mutation($w:ID!,$n:String!,$q:JSON!){ createView(workspaceId:$w,name:$n,query:$q,columns:[],isShared:true){id name} }',
    { w: wsId, n: 'deep-test-view', q: { and: [] } });
  return { slug, wsId, viewId: v.createView.id };
}
function injectTokenCtx(ctx, token) {
  return ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
}

const CASES = [];

// 准备：导航 + 等待 hydration（每个 case 都重做）
async function navigate(browser, token, slug, viewId) {
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  await page.waitForFunction(
    () => Array.from(document.querySelectorAll('.wside .it')).filter(el => {
      const s = el.getAttribute('style');
      return s && s.includes('padding-left');
    }).length >= 1,
    { timeout: 120000, polling: 500 },
  );
  // 先点一下 deep-test-view 让它变成 active_view（这样 name input 会显示）
  await page.evaluate((name) => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    for (const r of rows) {
      if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes(name)) {
        r.click();
        return;
      }
    }
  }, 'deep-test-view');
  await page.waitForTimeout(500);
  return { ctx, page };
}

// D1: 弹窗后 t100 / t500 / t1500 都可见（防御"对话框闪现后消失"）
// 用 mouse.click() 选 view（不是 r.click()）让 on:click 真实触达；然后 right-click。
CASES.push(async ({ browser, token, slug, viewId }) => {
  const t = Date.now();
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  await page.waitForFunction(
    () => Array.from(document.querySelectorAll('.wside .it')).filter(el => {
      const s = el.getAttribute('style');
      return s && s.includes('padding-left');
    }).length >= 1,
    { timeout: 120000, polling: 500 },
  );
  const rowBox = await page.evaluate(() => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    for (const r of rows) {
      if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes('deep-test-view')) {
        const b = r.getBoundingClientRect();
        return { x: b.x, y: b.y, w: b.width, h: b.height };
      }
    }
    return null;
  });
  if (!rowBox) { await ctx.close(); return fail('D1 三档稳定', 'row not found'); }
  // 先左键选 view，再右键
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2));
  await page.waitForTimeout(300);
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2), { button: 'right' });
  await page.waitForTimeout(200);
  const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
  if (!cfgBtn) { await ctx.close(); return fail('D1 三档稳定', 'no 配置视图 button'); }
  await cfgBtn.click();
  await page.waitForTimeout(100);
  const t100 = await page.evaluate(() => !!document.querySelector('.dmodal'));
  await page.waitForTimeout(400);
  const t500 = await page.evaluate(() => !!document.querySelector('.dmodal'));
  await page.waitForTimeout(1000);
  const t1500 = await page.evaluate(() => !!document.querySelector('.dmodal'));
  await ctx.close();
  if (t100 && t500 && t1500) return pass('D1 三档稳定', Date.now() - t);
  return fail('D1 三档稳定', `t100=${t100} t500=${t500} t1500=${t1500}`);
});

// D2: 弹窗内容结构正确（h3 标题、3 个时间轴 select、share checkbox、取消 + 保存）
CASES.push(async ({ browser, token, slug, viewId }) => {
  const t = Date.now();
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  await page.waitForFunction(
    () => Array.from(document.querySelectorAll('.wside .it')).filter(el => {
      const s = el.getAttribute('style');
      return s && s.includes('padding-left');
    }).length >= 1,
    { timeout: 120000, polling: 500 },
  );
  const rowBox = await page.evaluate(() => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    for (const r of rows) {
      if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes('deep-test-view')) {
        const b = r.getBoundingClientRect();
        return { x: b.x, y: b.y, w: b.width, h: b.height };
      }
    }
    return null;
  });
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2));
  await page.waitForTimeout(300);
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2), { button: 'right' });
  await page.waitForTimeout(200);
  const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
  await cfgBtn.click();
  await page.waitForTimeout(500);
  const c = await page.evaluate(() => {
    const dmbox = document.querySelector('.dmodal .dmbox');
    if (!dmbox) return null;
    return {
      h3: dmbox.querySelector('h3')?.innerText,
      selectCount: dmbox.querySelectorAll('select').length,
      hasShareCheckbox: !!dmbox.querySelector('input[type="checkbox"]'),
      hasCancel: !!Array.from(dmbox.querySelectorAll('button')).find(b => b.innerText.match(/取消/)),
      hasSave: !!Array.from(dmbox.querySelectorAll('button')).find(b => b.innerText.match(/保存/)),
    };
  });
  await ctx.close();
  if (c && c.h3?.includes('配置视图') && c.selectCount === 3 && c.hasShareCheckbox && c.hasCancel && c.hasSave) {
    return pass('D2 dialog 结构', Date.now() - t);
  }
  return fail('D2 dialog 结构', JSON.stringify(c));
});

// D3: 改名字 + 保存，刷新后名字真的变了（验证表单能交互能落库）
CASES.push(async ({ browser, token, slug, viewId }) => {
  const t = Date.now();
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  await page.waitForFunction(
    () => Array.from(document.querySelectorAll('.wside .it')).filter(el => {
      const s = el.getAttribute('style');
      return s && s.includes('padding-left');
    }).length >= 1,
    { timeout: 120000, polling: 500 },
  );
  const rowBox = await page.evaluate(() => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    for (const r of rows) {
      if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes('deep-test-view')) {
        const b = r.getBoundingClientRect();
        return { x: b.x, y: b.y, w: b.width, h: b.height };
      }
    }
    return null;
  });
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2));
  await page.waitForTimeout(300);
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2), { button: 'right' });
  await page.waitForTimeout(200);
  const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
  await cfgBtn.click();
  await page.waitForSelector('.dmodal .dmbox input.inp[placeholder*="视图"]', { timeout: 10000 });
  const nameInput = await page.$('.dmodal .dmbox input.inp[placeholder*="视图"]');
  if (!nameInput) { await ctx.close(); return fail('D3 改名保存', 'no name input'); }
  const newName = '重命名-' + Date.now().toString(36).slice(-4);
  await nameInput.click({ clickCount: 3 });
  await nameInput.fill(newName);
  const saveBtn = await page.$('.dmodal .dmbox button:has-text("保存")');
  await saveBtn.click();
  await page.waitForTimeout(1000);
  const dmodalAfter = await page.evaluate(() => !!document.querySelector('.dmodal'));
  const newNameVisible = await page.evaluate((n) => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    return rows.some(r => r.innerText.includes(n));
  }, newName);
  await ctx.close();
  if (!dmodalAfter && newNameVisible) return pass('D3 改名保存', Date.now() - t);
  return fail('D3 改名保存', `dialog=${dmodalAfter} newNameInSidebar=${newNameVisible} (期望 "${newName}")`);
});

// D4: 取消按钮不修改视图
CASES.push(async ({ browser, token, slug, viewId }) => {
  const t = Date.now();
  // 通过 API 取这个 view 当前的名字（D3 改名后这里反映最新名字）
  const wsInfo = await gql(token, 'query($s:String!){ workspace(slug:$s){id} }', { s: slug });
  const v = await gql(token, 'query($w:ID!){ views(workspaceId:$w){id name} }',
    { w: wsInfo.workspace.id });
  const myView = v.views.find(x => x.id === viewId);
  const currentName = myView ? myView.name : 'deep-test-view';
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  await page.waitForFunction(
    () => Array.from(document.querySelectorAll('.wside .it')).filter(el => {
      const s = el.getAttribute('style');
      return s && s.includes('padding-left');
    }).length >= 1,
    { timeout: 120000, polling: 500 },
  );
  const rowBox = await page.evaluate((name) => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    for (const r of rows) {
      if ((r.getAttribute('style') || '').includes('padding-left') && r.innerText.includes(name)) {
        const b = r.getBoundingClientRect();
        return { x: b.x, y: b.y, w: b.width, h: b.height };
      }
    }
    return null;
  }, currentName);
  if (!rowBox) { await ctx.close(); return fail('D4 取消不改', `row not found for "${currentName}"`); }
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2));
  await page.waitForTimeout(300);
  await page.mouse.click(Math.round(rowBox.x + rowBox.w / 2), Math.round(rowBox.y + rowBox.h / 2), { button: 'right' });
  await page.waitForTimeout(200);
  const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
  await cfgBtn.click();
  await page.waitForSelector('.dmodal .dmbox input.inp[placeholder*="视图"]', { timeout: 10000 });
  const nameInput = await page.$('.dmodal .dmbox input.inp[placeholder*="视图"]');
  const shouldNotPersist = 'SHOULD-NOT-PERSIST-' + Date.now().toString(36).slice(-6);
  await nameInput.click({ clickCount: 3 });
  await nameInput.fill(shouldNotPersist);
  const cancelBtn = await page.$('.dmodal .dmbox button:has-text("取消")');
  await cancelBtn.click();
  await page.waitForTimeout(500);
  const dmodalAfter = await page.evaluate(() => !!document.querySelector('.dmodal'));
  const notPersisted = await page.evaluate((name) => {
    const rows = Array.from(document.querySelectorAll('.wside .it'));
    return !rows.some(r => r.innerText.includes(name));
  }, shouldNotPersist);
  await ctx.close();
  if (!dmodalAfter && notPersisted) return pass('D4 取消不改', Date.now() - t);
  return fail('D4 取消不改', `dialog=${dmodalAfter} notPersisted=${notPersisted} (name was "${currentName}")`);
});

// D5: 基础视图的 配置视图 弹窗无 name input、无 delete 按钮（基础视图不可改名不可删）
CASES.push(async ({ browser, token, slug, viewId }) => {
  const t = Date.now();
  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  const page = await ctx.newPage();
  try {
    await page.goto(`${SERVER}/${slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  await page.waitForFunction(
    () => document.querySelector('.wside .it.base'),
    { timeout: 120000, polling: 500 },
  );
  const baseBox = await page.evaluate(() => {
    const el = document.querySelector('.wside .it.base');
    if (!el) return null;
    const b = el.getBoundingClientRect();
    return { x: b.x, y: b.y, w: b.width, h: b.height };
  });
  if (!baseBox) { await ctx.close(); return fail('D5 基础视图', 'base row not found'); }
  await page.mouse.click(Math.round(baseBox.x + baseBox.w / 2), Math.round(baseBox.y + baseBox.h / 2), { button: 'right' });
  await page.waitForTimeout(200);
  const cfgBtn = await page.$('.vmenu button:has-text("配置视图")');
  if (!cfgBtn) { await ctx.close(); return fail('D5 基础视图', 'no 配置视图 button on base view'); }
  await cfgBtn.click();
  await page.waitForTimeout(500);
  const result = await page.evaluate(() => {
    const dmbox = document.querySelector('.dmodal .dmbox');
    if (!dmbox) return null;
    return {
      hasDmbox: true,
      selectCount: dmbox.querySelectorAll('select').length,
      nameInputCount: dmbox.querySelectorAll('input.inp[placeholder*="视图"]').length,
      hasDeleteBtn: !!Array.from(dmbox.querySelectorAll('button')).find(b => b.innerText.match(/删除视图/)),
    };
  });
  await ctx.close();
  if (result && result.hasDmbox && result.selectCount === 3 && result.nameInputCount === 0 && !result.hasDeleteBtn) {
    return pass('D5 基础视图', Date.now() - t);
  }
  return fail('D5 基础视图', JSON.stringify(result));
});

(async () => {
  console.log('[' + nowIso() + '] launching webkit');
  const pwCore = pw.default || pw;
  const browser = await pwCore.webkit.launch({ headless: true });
  let token;
  try {
    token = await loginToken();
    console.log('[' + nowIso() + '] login OK, token len=' + token.length);
  } catch (e) { console.error('login failed: ' + e.message); process.exit(2); }
  let bs;
  try {
    bs = await bootstrap(token);
    console.log(`[${nowIso()}] bootstrap ws=${bs.slug} view=${bs.viewId}`);
  } catch (e) { console.error('bootstrap failed: ' + e.message); process.exit(2); }

  const results = [];
  for (const c of CASES) {
    const r = await c({ browser, token, slug: bs.slug, viewId: bs.viewId });
    results.push(r);
    const tag = r.pass ? 'PASS' : 'FAIL';
    const extra = r.pass ? `${r.ms}ms` : r.fail + (r.extra ? ' ' + JSON.stringify(r.extra) : '');
    console.log('[' + nowIso() + '] ' + tag + ' ' + r.name + ' — ' + extra);
  }
  await browser.close();

  const failed = results.filter(r => !r.pass);
  console.log('');
  console.log('=== 汇总 ===');
  console.log('total=' + results.length + ' pass=' + (results.length - failed.length) + ' fail=' + failed.length);
  if (failed.length) {
    for (const f of failed) console.log('  - ' + f.name + ': ' + f.fail);
    process.exit(1);
  }
})();