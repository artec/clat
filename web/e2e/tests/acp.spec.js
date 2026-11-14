const { test, expect } = require('@playwright/test');
const { hostInfo, openWorkbench, LIVE } = require('../helpers/workbench');

test('ACP composer delegates only the confirmed task and keeps a newer draft', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  await page.route('**/api/acp.agents.list', route => route.fulfill({ json: {
    ok:true, value:[{id:'fixture',label:'Fixture Agent',trusted:true}],
  } }));
  let accepted;
  let release;
  await page.route('**/api/acp.delegations.start', async route => {
    accepted = route.request().postDataJSON();
    await new Promise(resolve => { release = resolve; });
    await route.fulfill({ json:{ok:true,value:{id:'delegation'}} });
  });
  await page.route('**/api/acp.delegations.list', route => route.fulfill({ json:{ok:true,value:[]} }));
  await page.locator('#prompt').fill('Only send this task.');
  await page.locator('#composer-add').click();
  await page.locator('#add-acp-delegation').click();
  const dialog = page.locator('dialog[open].session-share-dialog');
  await expect(dialog.locator('textarea')).toHaveValue('Only send this task.');
  await dialog.getByRole('button', {name:'Start',exact:true}).click();
  await expect.poll(() => accepted?.text).toBe('Only send this task.');
  expect(accepted.continueWithClat).toBe(true);
  expect(accepted.agent).toBe('fixture');
  await page.evaluate(() => {
    const input = document.querySelector('#prompt');
    input.value = 'A newer unsent draft.';
    input.dispatchEvent(new Event('input', {bubbles:true}));
  });
  release();
  await expect(dialog).toHaveCount(0, LIVE);
  await expect(page.locator('#prompt')).toHaveValue('A newer unsent draft.');
});

test('ACP external session sends negotiated attachments and shows plan updates', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  await page.route('**/api/acp.agents.list', route => route.fulfill({ json:{ok:true,value:[{id:'agent',label:'Fixture',trusted:true}]} }));
  await page.route('**/api/acp.sessions.start', route => route.fulfill({ json:{ok:true,value:{id:'external'}} }));
  await page.route('**/api/acp.sessions.status', route => route.fulfill({ json:{ok:true,value:{
    phase:'idle', capabilities:{promptCapabilities:{image:true}}, updates:[{
      sessionUpdate:'plan', entries:[{content:'Inspect the attachment',status:'in_progress'}],
    }],
  }} }));
  let accepted;
  await page.route('**/api/acp.sessions.action', route => {
    accepted = route.request().postDataJSON();
    return route.fulfill({json:{ok:true,value:{id:'external'}}});
  });
  await page.locator('[data-action="new-external-session"]').first().click();
  await page.getByRole('button', {name:'Start',exact:true}).click();
  const dialog = page.locator('dialog[data-acp-session="external"][open]');
  await expect(dialog.getByText('Inspect the attachment', {exact:false})).toBeVisible();
  await dialog.locator('input[type=file]').setInputFiles({name:'test.png',mimeType:'image/png',buffer:Buffer.from('test')});
  await dialog.getByRole('button', {name:'Send',exact:true}).click();
  await expect.poll(() => accepted?.content).toEqual([{type:'image',mimeType:'image/png',data:'dGVzdA=='}]);
  expect(accepted.text).toBe('');
});
