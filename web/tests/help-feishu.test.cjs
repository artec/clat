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
  'globalThis.helpTopics=HELP_TOPICS;',
  'globalThis.helpTopicFor=typeof settingsHelpTopic==="function"?settingsHelpTopic:null;',
].join('\n');
vm.runInContext(source,context,{filename:'help-feishu-test.js'});

test('Feishu help is a separate guide and Remote follows the selected channel',()=>{
  const feishu=context.helpTopics.find(topic=>topic.id==='feishu');
  const telegram=context.helpTopics.find(topic=>topic.id==='telegram');
  assert.equal(telegram.group,'settings');
  assert.equal(telegram.target,'settings:telegram');
  const wechat=context.helpTopics.find(topic=>topic.id==='wechat');
  assert.equal(feishu.group,'settings');
  assert.equal(feishu.target,'settings:feishu');
  assert.ok(feishu.related.includes('wechat'));
  assert.ok(wechat.related.includes('feishu'));
  assert.equal(context.helpTopicFor('remote','feishu'),'feishu');
  assert.equal(context.helpTopicFor('remote','telegram'),'telegram');
  assert.equal(context.helpTopicFor('remote','wechat'),'wechat');
  assert.equal(context.helpTopicFor('remote'),'wechat');
  assert.equal(context.helpTopicFor('models'),'models');
  for(const language of ['en','zh-Hans','zh-Hant']){
    const catalog=JSON.parse(fs.readFileSync(path.join(root,'locales',language+'.json'),'utf8'));
    for(const suffix of ['title','summary','step_1','step_2','step_3','question','answer']){
      const text=catalog['help.feishu_'+suffix];
      assert.equal(typeof text,'string',language+' help.feishu_'+suffix);
      assert.equal(text.includes('<'),false);
    }
    assert.match(catalog['help.feishu_step_1'],/im:message\.p2p_msg:readonly/);
    assert.match(catalog['help.feishu_step_1'],/im:message:send_as_bot/);
    assert.match(catalog['help.feishu_step_3'],/im\.message\.receive_v1/);
    assert.match(catalog['help.feishu_step_3'],/使用长连接接收事件/);
    for(const suffix of ['title','summary','step_1','step_2','step_3','question','answer']){
      const text=catalog['help.telegram_'+suffix];
      assert.equal(typeof text,'string',language+' help.telegram_'+suffix);
      assert.equal(text.includes('<'),false);
    }
    assert.match(catalog['help.telegram_step_1'],/BotFather/);
    assert.match(catalog['help.telegram_step_1'],/\/newbot/);
    assert.match(catalog['help.telegram_step_3'],/\/pair/);
  }
});
