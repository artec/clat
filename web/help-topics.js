/* Local guide metadata; all displayed copy lives in the shared catalogs. */
const HELP_TOPICS = [
  // 对话与运行
  {"id":"chat","group":"conversation","icon":"chat","target":"conversation","related":["images","queue","sessions","share","reply_links","context","keyboard"]},
  {"id":"images","group":"conversation","icon":"vision","target":"conversation","questions":2,"related":["chat","models","context"]},
  {"id":"queue","group":"conversation","icon":"pending","target":"conversation","related":["chat","workflow","keyboard"]},
  {"id":"sessions","group":"conversation","icon":"archive","target":"conversation","questions":2,"related":["chat","share","history"]},
  {"id":"share","group":"conversation","icon":"chat","target":"conversation","related":["chat","sessions","history"]},
  {"id":"reply_links","group":"conversation","icon":"open","target":"conversation","related":["chat","files"]},
  {"id":"context","group":"conversation","icon":"trace","target":"conversation","related":["chat","images","models"]},
  {"id":"keyboard","group":"conversation","icon":"help","target":"conversation","related":["chat","files","git"]},

  // 工作区工具（与右栏 WORKSPACE_TOOLS 顺序一致，模型交接紧随计划与目标，后接记忆与子 Agent）
  {"id":"files","group":"tools","icon":"folder","target":"file-browser","related":["git","resources","terminal"]},
  {"id":"git","group":"tools","icon":"branch","target":"workspace-review","related":["git_settings","files","operations"]},
  {"id":"operations","group":"tools","icon":"activity-log","target":"turn-review","related":["git","processes"]},
  {"id":"processes","group":"tools","icon":"process-logs","target":"task-review","related":["tasks","operations","terminal"]},
  {"id":"workflow","group":"tools","icon":"plan","target":"workflow-details","related":["handoff","queue","permissions","tasks"]},
  {"id":"handoff","group":"tools","icon":"handoff","target":"settings:utility","related":["workflow","utility","models","authorizations"]},
  {"id":"memory_subagents","group":"tools","icon":"brain","target":"conversation","questions":2,"related":["workflow","skills","extensions","permissions"]},

  // 导航页面（与左全局导航栏顺序一致）
  {"id":"terminal","group":"pages","icon":"terminal","target":"terminal-page","related":["terminal_font","processes","projects"]},
  {"id":"resources","group":"pages","icon":"folder","target":"resources-dialog","related":["files","history"]},
  {"id":"tasks","group":"pages","icon":"schedule","target":"jobs-dialog","related":["processes","workflow"]},
  {"id":"history","group":"pages","icon":"history","target":"history-dialog","related":["chat","sessions","resources","operations"]},
  {"id":"extensions","group":"pages","icon":"extensions","target":"market-dialog","related":["skills","mcp","memory_subagents","permissions","network"]},

  // 设置与排障（严格对齐设置页左侧栏：工作台 → 能力 → 集成 → 应用）
  {"id":"appearance","group":"settings","icon":"settings","target":"settings:appearance","related":["terminal_font","chat","about"]},
  {"id":"terminal_font","group":"settings","icon":"terminal","target":"settings:terminal","related":["terminal","appearance"]},
  {"id":"projects","group":"settings","icon":"folder","target":"settings:projects","related":["chat","files","about"]},
  {"id":"models","group":"settings","icon":"chip","target":"settings:models","related":["authorizations","images","utility","handoff","context"]},
  {"id":"authorizations","group":"settings","icon":"key","target":"settings:authorizations","questions":3,"related":["models","handoff","utility"]},
  {"id":"utility","group":"settings","icon":"sparkles","target":"settings:utility","related":["handoff","models","git_settings","chat"]},
  {"id":"skills","group":"settings","icon":"book-open","target":"settings:skills","related":["keyboard","mcp","extensions"]},
  {"id":"permissions","group":"settings","icon":"permission-write","target":"settings:permissions","related":["workflow","files","terminal","computer-use"]},
  {"id":"computer-use","group":"settings","icon":"monitor-pointer","target":"settings:computer-use","related":["permissions","about"]},
  {"id":"mcp","group":"settings","icon":"mcp","target":"settings:mcp","related":["skills","extensions","permissions"]},
  {"id":"acp","group":"settings","icon":"task-handoff","target":"settings:acp","related":["permissions","tasks"]},
  {"id":"network","group":"settings","icon":"search","target":"settings:web","related":["models","extensions","wechat"]},
  {"id":"wechat","group":"settings","icon":"wechat","target":"settings:wechat","related":["feishu","telegram","permissions","network"]},
  {"id":"feishu","group":"settings","icon":"feishu","target":"settings:feishu","related":["wechat","telegram","permissions","network"]},
  {"id":"telegram","group":"settings","icon":"telegram","target":"settings:telegram","related":["wechat","feishu","permissions","network"]},
  {"id":"git_settings","group":"settings","icon":"branch","target":"settings:git","related":["git","utility","models"]},
  {"id":"about","group":"settings","icon":"info","target":"settings:desktop","related":["projects","appearance","models"]},
];
