const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench}=require('../helpers/workbench');

test('Settings offers independent user/project Skill creation and removal, not an Extensions tab',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.click('#nav-settings');
  await page.locator('[data-settings-category="skills"]').click();
  const panel=page.locator('[data-settings-panel="skills"]');
  await expect(panel).toBeVisible();
  await expect(page.locator('#market-dialog')).toBeHidden();
  await panel.locator('button').filter({hasText:'New Skill'}).click();
  await panel.locator('input').first().fill('managed-e2e-review');
  await panel.locator('input').nth(1).fill('Browser-managed skill');
  await panel.locator('textarea').fill('Review every change and cite actual source lines.');
  await panel.locator('button.primary').click();
  await expect.poll(()=>page.evaluate(async()=> (await rpc('skill.manage.list',{})).user.some(e=>e.name==='managed-e2e-review'))).toBe(true);
  const source=await page.evaluate(async()=>rpc('interaction.catalog',{}));
  expect(source.skills.entries.some(e=>e.name==='managed-e2e-review')).toBe(true);
  const row=panel.locator('.settings-row').filter({hasText:'managed-e2e-review'}).first();
  page.once('dialog',d=>d.accept());
  await row.getByRole('button',{name:'Delete'}).click();
  await expect.poll(()=>page.evaluate(async()=> (await rpc('skill.manage.list',{})).user.some(e=>e.name==='managed-e2e-review'))).toBe(false);
});
test('MCP settings manage disabled server without starting a process',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.click('#nav-settings');await page.locator('[data-settings-category="mcp"]').click();
  const panel=page.locator('[data-settings-panel="mcp"]');await expect(panel).toBeVisible();
  await panel.getByRole('button',{name:'Add MCP server'}).click();
  await panel.locator('input').first().fill('managed-e2e-disabled');
  await panel.locator('input').nth(1).fill('unavailable-fixture-for-ui-test');
  await panel.locator('input[type=checkbox]').uncheck();
  await panel.locator('button.primary').click();
  await expect.poll(()=>page.evaluate(async()=>{
    const v=await rpc('mcp.manage.list',{project_root:state.workbench.project.root});
    return v.user.some(e=>e.name==='managed-e2e-disabled'&&e.enabled===false);
  })).toBe(true);
  const v=await page.evaluate(async()=>rpc('mcp.manage.list',{project_root:state.workbench.project.root}));
  expect(JSON.stringify(v)).not.toContain('unavailable-fixture-for-ui-test');
  page.once('dialog',d=>d.accept());
  await panel.locator('.settings-row').filter({hasText:'managed-e2e-disabled'}).getByRole('button',{name:'Delete'}).click();
  await expect.poll(()=>page.evaluate(async()=>{
    const next=await rpc('mcp.manage.list',{project_root:state.workbench.project.root});
    return next.user.some(e=>e.name==='managed-e2e-disabled');
  })).toBe(false);
});
