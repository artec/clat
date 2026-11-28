const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');

const root=path.join(__dirname,'../..');
const context=vm.createContext({});
const source=[
  fs.readFileSync(path.join(root,'web/help-topics.js'),'utf8'),
  fs.readFileSync(path.join(root,'web/help-center.js'),'utf8'),
  'globalThis.HELP_TOPICS=HELP_TOPICS;',
  'globalThis.settingsHelpTopic=settingsHelpTopic;',
  'globalThis.resolveHelpTopic=resolveHelpTopic;',
  'globalThis.helpFaqSuffixes=helpFaqSuffixes;',
  'globalThis.helpSearchText=helpSearchText;',
].join('\n');
vm.runInContext(source,context,{filename:'help-center-test.js'});

// 校验帮助中心全部主题的图标、关联、跳转目标、设置页顺序与三语完整性
test('help topics satisfy structural, ordering and three-language invariants',()=>{
  const appSrc=fs.readFileSync(path.join(root,'web/app.js'),'utf8');
  const iconNames=new Set([...appSrc.matchAll(/^\s*(?:'([^']+)'|"([^"]+)"|([a-z0-9_-]+)):\s*\[/gm)].map(m=>m[1]||m[2]||m[3]));
  const topics=context.HELP_TOPICS;
  const ids=new Set(topics.map(t=>t.id));
  assert.equal(ids.size,topics.length,'topic ids must be unique');

  const validGroups=new Set(['conversation','tools','pages','settings']);
  const settingsOrder=['appearance','terminal','projects','models','authorizations','utility','skills','permissions','computer-use','mcp','acp','web','wechat','feishu','telegram','git','desktop'];
  const validTools=new Set(['file-browser','workspace-review','turn-review','task-review','workflow-details']);
  const validPages=new Set(['terminal-page','resources-dialog','jobs-dialog','history-dialog','market-dialog']);

  let lastSettingsIdx=-1;
  for(const topic of topics){
    assert.ok(validGroups.has(topic.group),`invalid group: ${topic.group}`);
    assert.ok(iconNames.has(topic.icon),`missing icon ${topic.icon} for ${topic.id}`);
    for(const rel of topic.related){
      assert.ok(ids.has(rel),`missing related ${rel} in ${topic.id}`);
      assert.notEqual(rel,topic.id,`self-reference in ${topic.id}`);
    }
    if(topic.target.startsWith('settings:')){
      const category=topic.target.slice(9);
      assert.ok(settingsOrder.includes(category),`unknown settings target ${topic.target}`);
      if(topic.group==='settings'){
        const idx=settingsOrder.indexOf(category);
        assert.ok(idx>=lastSettingsIdx,`settings topic order drifted at ${topic.id}`);
        lastSettingsIdx=idx;
      }
    }else if(topic.target!=='conversation'){
      assert.ok(validTools.has(topic.target)||validPages.has(topic.target),`unknown target ${topic.target}`);
    }
  }

  // 确认设置分类映射、授权中心三合一及旧配额链接别名兼容
  for(const cat of ['appearance','terminal','projects','models','authorizations','utility','skills','permissions','computer-use','mcp','acp','web','remote','git','desktop']){
    assert.ok(ids.has(context.settingsHelpTopic(cat)),`unmapped settings category ${cat}`);
  }
  assert.equal(context.settingsHelpTopic('authorizations'),'authorizations');
  assert.equal(context.settingsHelpTopic('remote','telegram'),'telegram');
  const telegram=topics.find(t=>t.id==='telegram');
  assert.ok(telegram.related.includes('network'),'telegram guide should link network settings');
  // 微信、飞书、Telegram 三个远程通道图标须统一走 FILL_ICONS 实心官方轮廓管线
  const fillMatch=appSrc.match(/const FILL_ICONS = new Set\(\[([^\]]+)\]\)/);
  assert.ok(fillMatch,'FILL_ICONS definition missing');
  for(const brand of ['wechat','feishu','telegram']){
    assert.match(fillMatch[1],new RegExp(`'${brand}'`),`${brand} must be in FILL_ICONS`);
  }
  for(const oldId of ['quota_qianwen','quota_bailian','quota_tencent']){
    assert.equal(context.resolveHelpTopic(oldId),'authorizations');
  }

  // 校验所有主题（含多条 FAQ）的三语键均存在、非空且无 HTML/CR
  for(const lang of ['en','zh-Hans','zh-Hant']){
    const catalog=JSON.parse(fs.readFileSync(path.join(root,'locales',lang+'.json'),'utf8'));
    for(const topic of topics){
      const suffixes=['title','summary','step_1','step_2','step_3',...context.helpFaqSuffixes(topic).flat()];
      for(const suffix of suffixes){
        const key=`help.${topic.id}_${suffix}`;
        assert.equal(typeof catalog[key],'string',`${lang} missing ${key}`);
        assert.ok(catalog[key].trim().length>0,`${lang} empty ${key}`);
        assert.equal(catalog[key].includes('<'),false,`${lang} ${key} contains HTML`);
        assert.equal(catalog[key].includes('\r'),false,`${lang} ${key} contains CR`);
      }
    }
  }
});

// 校验多条 FAQ 会一并纳入帮助中心全文检索
test('helpSearchText indexes multi-FAQ entries for merged authorizations guide',()=>{
  const en=JSON.parse(fs.readFileSync(path.join(root,'locales/en.json'),'utf8'));
  context.CLATI18n={msg:key=>en[key]||''};
  const auth=context.HELP_TOPICS.find(t=>t.id==='authorizations');
  assert.equal(auth.questions,3);
  const haystack=context.helpSearchText(auth);
  assert.match(haystack,/127\.0\.0\.1/);
  assert.match(haystack,/qcloudhunyuanreadonlyaccess/);
  assert.match(haystack,/model api keys/);
});
