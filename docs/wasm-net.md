# WASM 网络宿主协议与交付（PLG-4 D1–D3）

状态：**D1–D3 已交付并通过独立审计，D3 独立终审于 2026-10-05 通过；生产搜索包 `io.artec.dsh-official-web-wasm` 已上线 pi.at.cn，2026-10-06 实测目录状态为 available。**

官方四件套 0.2.0-rc.2 的源码保持原样。第二风味以显式 context scope 替代
ALS，闭集 DNS/HTTP 适配消费宿主私有、单次使用的 resolution；网络、审批、
期限、取消和资源释放走同一权威链。锁定的原生任务桥引擎打入组件，终端用户
不需要安装新的运行时。同步 import 候选因冻结 guest 取消而淘汰，历史证据保留。

真实 Wasmtime 组件已验证原工具发现、provided WebRuntime 的 child scope 清理、
未声明域拒绝、原 DeepSeek 搜索 POST/响应读取、原 HTTP 成功抓取与同域重定向、
跨域拒绝、headers/body 等待时取消及物理连接关闭。第二风味还直接导出现有
`clat:plugin/tools@0.1.0`，通过原 tools/list、tools/call 及错误结果契约，
其 config import 仅由可信宿主提供插件自身配置。
Bun 原件与 WASM 宿主使用同一份 29 组公网/混私网/NAT64 六布局数据判别。

本地交付：作者 Rust 147 项普通测试通过，10 项需显式武装的测试不混算；
JS/发布隔离 23 项及原 Bun 配方 9 项通过；完整仓库门禁通过。
D1 的 24 行攻击有逐行删除红点与对应作者绿卷，供独立审计复核；
该账本不宣称穷尽全部攻击或取得生产验收。

**D3 当前交付范围是搜索限定成品**：网络库已共用于生产 loader 与作者宿主，
签名安装、v2 目录、PWA 同意与卸载、真实 DeepSeek 五次及 Bun 五次测量已通过。
原正式 WIT、v1 linker 注册集合、已发布 MCP 工件与 v1 签名索引字节不变。
identity、gzip（含多成员）、zlib/raw deflate、Brotli 已由共享原 HTTP provider 向量和实际正式组件核验。
精确 origin 限制依旧生效；首发搜索限定，任意 URL 抓取继续用原 MCP 入口。
独立审计、正式签名与 FTP 上线由负责人执行，不混入开发完成结论。

真实 DNS 按负责人决定免测；原 HTTP 成功卷以测试专属的固定数字回环路由运行，
宿主向原包投影固定公网测试地址，cfg(test) 外不存在该路由。它证明消费者与
资源链的语义，不证明公网连接或真实 DNS64 可用性；生产地址围栏未放宽。
完整的源码、构建与武装命令见 [第二风味说明](../sdk/dsh-wasm-flavor/README.md)
和 [作者宿主说明](../sdk/wit-proposals/README.md)。后文施工段落按历史阶段保留，
当前状态以本节为准。

## 决策与适用范围

历史设计选择独立 `clat:net/egress@0.1.0`，保留 `clat:plugin@0.1.0`；
`clat:net/egress` 是当时的候选名，生产接口已定名为 `clat:net-task@0.1.0`。
相比直接开放 wasi:http，此接口不接受 guest 的 socket、connector、代理或 IP
选择器，解析凭证与连接可以绑定到同一宿主资源。HTTP 请求体/响应体仍是 HTTP
实体数据，不是 TCP/UDP 或 Node stream 管道。实现只进入 Rust core，前端负责显示审批。

WIT import 没有“可选”开关：新组件显式导入 net；老组件不导入而照常装载。
新宿主可同时注册两组接口；旧宿主加载新组件必须拒绝缺失的 import，不能降级绕过。
组合 world 在验证时引用原 plugin world，不复制或升级原协议。
[WIT 参考](https://component-model.bytecodealliance.org/design/wit.html) 定义接口与资源形状；
下述行为是 CLAT 自己的规范，不由 WIT 类型检查代替。

当前 net 闭集只有 HTTP 与其 DNS 校验；不提供任意 DNS 查询、socket、代理、
进程、线程、监听、通用流、文件系统、系统环境或 Node 平台设施。
密码学原语、随机、时钟若确有需求，须分别做版本/能力决策；不能塞进 net。

**所有宿主出口都纳入组合审核。** 现有host.call-tool允许run_command，能借进程
绕过域名围栏。带network声明的v2包必须拒绝hostTools.run_command、任意WASI
preopen/外部目录授权及其他能引入进程/动态代码/无限网络的组合；不是再弹一次Execute
就把它称为沙箱。直接组件导入不等于授权，既有host.call-tool仍需按清单拒绝调用。
sampling是独立已声明能力，其模型provider出站不受net的origin列表约束：有sampling
的包须展示“受限HTTP/DNS + 宿主模型服务”，不能显示泛指全部网络的围栏承诺。
官方新flavor若不需要sampling则声明false；需要该能力时按原ctx语义验收并明示。

## 不变量

1. 每次实际 DNS/HTTP 出站先做 manifest/config 围栏和现有 Network 权限决策。
2. Full Access、Project Write 的自动允许不突破围栏；没有 active run、清单或声明即拒。
3. 连接的每个候选地址来自宿主自己的完整校验集合，guest 返回的 IP 没有授权效力。
4. 宿主不自动跟重定向，不继承代理、cookies、凭据、全局 dispatcher 或连接池。
5. 取消、资源销毁、超时、run 结束、策略代际变化是终态，迟到 DNS/审批结果不能复活。
6. 一个解析凭证只允许一次 HTTP 尝试；所有 HTTP body read 共享该尝试的绝对 deadline。
7. 密钥不进入环境快照、诊断、fuel 日志、审计事件、审批持久化或底层错误文本。
8. 计算 fuel 只覆盖 guest；宿主等待的有界性由 deadline、取消、资源预算保证。

## 围栏、版本与状态读写者

建议新网络包使用 manifest v2，`capabilities.network` 候选结构为：

```json
{"protocol":"clat:net@0.1.0","origins":[{"scheme":"https","host":"api.deepseek.com","port":443,"methods":["POST"]}]}
```

以上不是当前可用 manifest。只接受有限 exact origin，最多64项；不接受通配域、
任意域、后缀匹配、IP literal、user-info、path/query/fragment 或无界端口。
host 经同一个 URL/IDNA 规范器转 ASCII、小写；尾点、空白、控制字符、zone id 拒绝，
显式默认端口与省略默认端口归一；IP/非标准数值 IPv4 经 URL 规范化后仍拒绝。
HTTP(80) 必须显式声明；不默认降级 HTTPS。方法集合非空，config 只能求交收窄，
缺省继承清单、显式空集合拒绝全部；非法 config 不忽略，加载失败。

HTTP 抓取任意网页需要逐域扩围：网络沙箱版不提供隐式“互联网全部域”。
因而安装后它能抓取的网页是围栏子集；这是能力档差异，产品必须直接展示。
扩围须重新审核/用户确认清单版本，不能把一次运行审批当成改 manifest。

| 状态 | 写者 | 读者 | 失效点 |
|---|---|---|---|
| signed network 上限 | 新包发布者；安装事务经复核激活 | manifest loader、market review、host factory | 包版本替换 |
| config 收窄 | owner 的配置操作 | host factory；运行时不可接受 guest 扩围 | config revision 变化 |
| run 身份/取消 | core run lifecycle | 权限门、resolver、connector、body reader | clear、cancel、新 run |
| resolution/response | 当前 Store 的 ResourceTable | 同一插件/Store/run 的 net 操作 | drop、消费、超时、失效代际 |
| 首用审批缓存 | **v0 不新增** | 每次复用现有策略 | 无新持久批准状态 |

现有 manifest 与 market schema 都 deny-unknown-fields。不能在旧生产 index 中塞进
新 network 记录，否则旧客户端可能拒绝整份 index，伤及 Bun 兜底。
D3 须先实现新增 v2 索引端点/读取分支，原 v1 index、旧包和签名根保持；
新客户端可在 v2 中看到两种 flavor。能力 diff、安装确认、升级/卸载与 PWA 卡
都需纳入变更。legacy path-only WASM 不获得 net 能力。

## DNS、钉扎与 NAT64

`dns-resolve(origin, timeout)` 先验证规范 origin、active run、能力、请求预算、
Network 策略，再读取 A/AAAA。最多32条地址；空集、非法项、超量或**任何**非公网
项导致整单拒绝，不是滤掉私网后继续。CNAME 只作为该 origin 的解析链，不能改变
HTTP authority；最终全部候选地址都受同样检查。

地址分类使用钉版 IANA 特殊用途表并覆盖 DSH 的限制，不只检查“不是 RFC1918”。
拒 loopback、link-local、private、unspecified、multicast、documentation、benchmark、
保留地址及 IPv4 mapped/兼容/6to4/Teredo 等转换逃逸。明确新增表的审计版本，
不在请求中临时下载表。[IANA IPv4 表](https://www.iana.org/assignments/iana-ipv4-special-registry/)
与 [IPv6 表](https://www.iana.org/assignments/iana-ipv6-special-registry/) 是源依据。
作者候选固定2026-10-04下载的XML与派生CIDR表（IANA更新日期2025-10-09），
校验命令在作者SDK说明中。候选保守拒绝表内全部特殊用途范围，包括部分标注为
可全球到达的例外；短NAT64布局还拒非零保留octet/suffix。这些收窄已单独记录
DSH差异，尚不构成官方原件兼容或生产策略变更的结论。

上游对 IPv6 集合还查询 `ipv4only.arpa`，按 RFC6052 的32/40/48/56/64/96位布局
辨认 NAT64 内嵌 IPv4。简单拒绝未声明的发现域会使原安全路径失效。
本协议把 RFC7050 **固定发现查询**作为已批准 origin 解析中的宿主子操作，
不让 guest 指定发现域/答案/前缀；该固定查询须出现在审批说明与安全审核中。
发现答案是 DNS 元数据，不得用于创建 HTTP 凭证；getter `dns64-answers()` 返回
真实同次发现结果，使映射层可满足原件检查。宿主自己也检查所有 NAT64 内嵌地址。
发现失败、格式异常或无法安全裁决的 IPv6 集合拒绝整单，不伪造“没有 NAT64”。
[RFC6052](https://www.rfc-editor.org/rfc/rfc6052) 和
[RFC7050](https://www.rfc-editor.org/rfc/rfc7050) 是该例外的规范依据。

返回 resolution 资源：私有字段包括规范 origin、完整 pin 集、解析/发现结果、
插件/Store/run/policy revision、绝对截止、unused 状态。getter 返回副本。
`http-request` 只接受此资源，校验 URL origin 一致和方法围栏，原子 unused→consumed。
连接不再次 DNS 查询；重试只可用同 pin 集，不能回退默认 connector。Host/TLS SNI
仍是 URL hostname，证书链及 hostname 正常校验。失败不会退回 unused。
“原子”是验证与使用同一权威 pin 集，不是承诺 DNS→TCP 中间没有时间流逝。

## HTTP 与实体生命周期

request URL 最大2048 UTF-8 bytes；无 user-info/fragment/control，scheme仅 HTTP(S)。
header 数≤64、总字节≤32KiB，拒 CR/LF/NUL、非法 token、重复敏感头以及
Host、Connection、Upgrade、Transfer-Encoding、Content-Length、Proxy-* 等控制头。
完整拒绝集合为上述名字（ASCII大小写不敏感）以及 Keep-Alive、TE、Trailer、Expect；
Proxy-* 表示任意以 proxy- 开头的名字。HTTP库自行产生必需的framing头。
宿主独占 authority/length/framing，不开放 CONNECT/TRACE/升级/WebSocket。
header name 大小写不改变规则；Authorization 等应用头允许但始终按秘密处理。

v0 每个请求体≤1MiB；每个响应 encoded≤8MiB、decoded≤8MiB；guest 的响应上限
只能收窄。状态/header 先于 body 返回，body 每次read≤64KiB、累计有界，含空响应。
服务器没有 Content-Length、chunked、撒谎长度或压缩炸弹都不能逃总量/期限闸。
gzip/deflate/br 若被实现，解码仍由 bounded HTTP entity 负责，不提供 node:zlib；
未支持的编码必须报 unsupported。完整兼容宣称前必须补齐原 provider 编码卷，
不能把拒绝该编码当成等价通过。

3xx 原样返回（manual），Location 是响应元数据。guest 每一 hop 必须重新
dns-resolve→http-request、重新过围栏/权限/钉扎；先前 pin 不可复用到另一 origin。
DSH 原件仍执行它自己的 same-origin/次数限制；宿主额外施加工具调用期最多10次
HTTP 尝试预算和共享总截止，避免 redirect 或纯JS循环换资源延长工作。
不能自动转发 Authorization/Cookie 跨 origin；v0 没有 cookie jar 或环境代理。
插件显式配置 proxy 的入口须拒绝 unsupported，不悄悄忽略并直连。

## Permission、deadline、取消和秘密

每次 DNS/HTTP 真实出站都通过 `InteractivePermissionPolicy` 与注入 approver。
请求 effect 固定 Network，不信工具自报 pure；复用现有权限档决策（Read Only 请求
批准，Project Write/Full Access 可自动允许）。v0 不新增 ask-once 或跨 run 网络授权。
DNS64 是该解析内的固定、已展示子操作；不形成另一 guest 可调用的网络能力。
每次批准后、开始连接前和发布结果前复验 run/cancel/policy generation。

宿主持有不可变权威请求；审批显示规范 destination/method、非敏感 header、
可审阅正文和明确的脱敏位置。秘密 header 值、私有配置中标记的秘密及其出现在
URL/body 的原文均不能进入持久记录。审批显示值与执行值绑定到同一宿主对象，
不能让 guest 提交“展示A、执行B”。不对 secret 生成日志用 hash，不打印底层
transport error/URL/响应正文；WIT 失败仅为有限 enum。未知未标记秘密以及恶意
guest 主动把密钥编码进合法数据，不在“诊断脱敏”证明范围；不能宣传阻止插件自身泄露。
现有 approval durable producer 若需新字段须按 catalog 四闸通过，不能新开字符串分支。

解析进入时开始 absolute deadline，包含排队/审批/DNS/连接/header/body；
timeout须正且≤30s，body 请求取 min(解析截止, http入参截止, run工具整体截止)。
`read-body` 的max-bytes必须在1..65536内，否则invalid-request；空返回只代表EOF。
每个 run ≤64次 DNS/64次HTTP，单个 tool call整体≤120s，Store ≤16个活资源，
host总量预算另受进程级限额，防多个插件联合作业耗尽内存/线程。
候选进程闸：最多8个在途HTTP、128MiB网络缓冲预留，DNS最多8个worker与8个排队任务；
满额拒绝limit-exceeded，不创建新thread或无界队列。网络buffer预留在实际分配前扣减。
取消必须在≤250ms内让等待/连接/read返回，并关闭该操作持有的连接；过期 DNS
工作即使底层不能取消也只可进入**有界**宿主队列，结果丢弃，不能 detached无限增殖。
卸载/Store drop/run clear 都取消并释放资源；drop/cancel不需要出站权限。
底层不可取消的DNS任务只属于上述共享有界pool，不持有Store/完整run context/秘密；
Store关闭不等待该OS调用无限join。pool满会拒新请求，这是保守可用性退化，不能称无残留。
host阻塞时不消耗guest fuel，epoch只终止guest计算，不会自动取消Rust DNS/HTTP：
后者必须检查同一CancelToken。不得靠guest AbortController独立承担宿主取消。

同步WIT import还有独立限制：阻塞的host调用期间，guest的timer/microtask不能
据此假定会继续执行，因此本接口的run取消/absolute deadline证明不等于任意
JS AbortSignal来源已经等价。D2的第一项consumer判别须覆盖“guest定时/并行
continuation触发abort，而DNS/header/body仍在等待”。若不能把原signal期限和
取消传递给host且保留调度语义，应复审为受同样策略控制的异步HTTP接口或wasi:http
适配；不能伪造Node timers、轮询烧掉fuel或缩减原件取消行为来通过。2026-10-04 的真实组件判别已确认：DNS/header/body 同步等待期间，排队的
Promise continuation 无法执行 abort；三处都在 host 返回后才取消。本同步候选
不能进入第二风味实现。异步接口复审已批准；隔离探针已验证原生 timer/Promise
触发 HTTP 取消，但独立 DNS 桥和同一权威链仍须实现。本同步判别未覆盖计时器、
真实网络撤销或24项宿主安全卷，不是四件套兼容结论。

## 第二风味与能力闭集

127 是严格打包**诊断次数**，现场归并为28个模块名，不是127项API承诺。
去掉完整 Undici 传递图后再次按可达导入裁决，不能为这些诊断全部造 Node 平台。

| 依赖族 | D2方向 | 越线条件 |
|---|---|---|
| node:dns/promises、Undici HTTP叶子 | clat:net 映射；pin回调只能引用宿主resolution | 复刻Agent/socket/connector、接受guest IP集合 |
| global fetch、有限Response body读取 | 同一 net HTTP实体映射 | 暴露Node通用stream、隐式联网路径 |
| node:net isIP、path/url 纯计算 | 复用有限解析纯函数并列出闭集；无IO | 完整复刻Node模块或读取本机路径状态 |
| shim的AsyncLocalStorage | 显式作用域context/清理栈 | 全局可变栈、await后所有者串线 |
| crypto随机UUID等 | 单独能力决策，当前net不提供 | 伪随机常量或为了兼容复制Nodecrypto |
| fs/os/module/sqlite/worker_threads/streams/timers等 | 可证明死代码则移除打包依赖；活路径分类拒装/另协议决策 | 假FS、伪homedir、require/platform仿真、补Node设施 |

ALS 改造必须枚举工具、inject callback、effect迭代、EventBus waterfall、服务回调、
sampling/elicitation continuation、dispose各入口。给每个closure一个不可变scope
以及以该scope构建的ctx；不能只把一个全局currentScope赋值再await。
相同ctx identity若被上游观察、跨scope注册服务/事件、late callback teardown
都须与原Shim oracle对照。taskbook“最便宜”尚未被证明，不作为工期/等价前提。

## D1审计门与后续裁判卷

D1交付是可解析WIT、组合world形状、自审与攻击规格，**没有网络实现**。
语法绿不是SSRF/权限/取消绿。D2开工前设计审计需裁决：固定DNS64发现例外、
严格origin带来的抓取范围、v2市场隔离、秘密脱敏与审批绑定、资源/等待预算。
本规格的hard caps是候选值，D2测量后只能通过明确规格变更调整。

D2先写真实宿主pre-red判别：未声明/同尾缀域、HTTP降级/端口、混合公网私网DNS、
NAT64全部六布局、rebinding、伪pin、代理绕过、重定向跨围栏、秘密错误、取消
每个等待点、迟到批准、跨Store/run复用、资源耗尽、慢/无限/压缩body。
再写原Shim交错async所有权oracle和PLG-1同卷测试。D3才做独立包
`io.artec.dsh-official-web-wasm`、PLG-2 signed staging/PWA全卷、真实搜索重复测量。
当前仍以已发布Bun/MCP包使用官方四件套。

候选WIT验证（作者机器，复用PLG-3锁定工具链）：

```sh
npm ci --prefix sdk/dsh-wasm-spike --ignore-scripts
node sdk/wit-proposals/validate.mjs
```

验证只构造临时dummy组件并检查world/import形状，finally清理；不发网络请求、
不装包、不注册宿主接口。它不能替代上述真实host/consumer裁判卷。

D2同步取消判别的构建命令、预期失败与证据范围见
[作者探针说明](../sdk/wit-proposals/README.md)。这是一项明确的消费者红卷，
不作为全量门禁的异步网络验收。异步复审已放行有限 DNS/HTTP 原生任务桥，
仍禁止默认联网 handler、伪 Node timer、无界线程和 guest 授权 pin。

D2已增加core内部网络审批租约：复用实际PermissionPolicyFactory（包括Plan
ToolAccessGuard），DNS/HTTP审批固定Network效果，显示仅含action/origin/method。
租约绑定run epoch、权限与服务配置代际、派生CancelToken及工具绝对期限；
clear、新run、权限A→B→A、服务配置刷新、取消和过期均撤销旧租约，迟到allow
必须再次检查。权限journal写入失败不会发布新档位或撤销代际。
核心租约与执行器由16项测试及22项隔离行为删除验证覆盖（含配置刷新交错、
ToolAccessSlot 策略 revision、Plan 开关 ABA 与审批期间切入 Plan 的迟到 Allow）。
作者网络库现110项测试，已通过测试适配器消费真实租约与审批句柄。
默认core构建不导出test-support测试面，没有注册WIT联网入口。实际
实际工具调用期限来源、WIT任务/订阅资源仍待接线；D2未完成，
四件套继续通过原Bun/MCP机制使用。复现命令见作者探针说明。

core另提供私有非阻塞审批句柄：进程共享8worker/8排队，满额Busy；start/get
不调用阻塞approver。每次审批派生独立操作子token，Drop或失效结果读取会撤销
它，保留父run与其他操作；结果单次消费前再次验证租约。排队期限沿用原工具
deadline。该层新增5项测试，另1项锁定子审批期限不超过工具租约；HTTP作者后端
通过仅测试构建可用的适配器消费句柄。阻塞且不响应cancel的
宿主approver可占住worker，固定容量会保守拒新请求，不创建补偿线程。
未来异步网络等待须用宿主调度观察句柄与租约，不能由guest轮询承担取消。


作者 DNS 入口现先完成真实 core Resolve 审批，再提交固定 DNS 池；准入时确定的
绝对期限包含审批等待。排队派发、RFC7050 发现前、结果读取与已消费 pin 均检查
同一租约；失效等待由宿主定时器观察。工作项只持弱 scope/slot，不保留完整 run。
新增9项离线 fake Lookup 测试、8项接线删除点；既有21项 DNS 删除点已复验。
Plan 策略发布同样撤销旧 DNS/HTTP 授权，包括等待中的审批和 header/body socket。
这些是作者接线证据，未增加正式 WIT 入口或完成24项总卷；真实 DNS 按负责人免做。


作者工具/分块后端现补齐：host建立的 Tool 绑定不可重置的绝对截止，共享10次
HTTP尝试；run共享64 DNS/64 HTTP，DNS凭证与HTTP预约共用Store16个逻辑资源上限。
NetworkScope.begin_tool 接收host期限，DNS等待与HTTP请求取原期限的最小值。
该入口尚未接生产工具invoke，正式任务、订阅与ResourceTable计数还需统一。
私有StreamResponse先返回状态/头，read参数为1..65536；总实体≤8MiB且guest可收窄，
空读取仅为EOF，错误保持终态。正常完成driver仍取已送达headers；逐次metadata/read
复验权威，失效metadata读取也关闭响应。显式cancel、response drop、等待read future
被销毁均关闭socket、释放实体预约；未增加分离后台任务。旧聚合接口复用此实现。
88项作者测试含13项新增预算/分块判别，无真实DNS、无四件套WASM兼容宣称。
闲置响应仍由下一次使用或资源销毁清理，正式run/Store卸载须销毁全部资源；
这部分WIT所有权和统一任务接线未完成，D2仍未完成。


作者 typed task/资源 owner 现以真实 Wasmtime ResourceTable 保存任务、resolution、
response与WASI pollable；每个实际entry计入16上限，包括订阅。句柄验证owner、
代际与类型，拒绝错类型销毁及回收旧句柄；删除父任务先清订阅。DNS/HTTP/read
共用同一Task/Pollable机制，只在宿主等待中推进future，没有新后台spawn。
get不等待、终态单消费；任务cancel/drop丢弃进行中的操作及未消费结果。
WASI ready/poll丢弃一次等待不会取消任务；显式拥有操作的等待future销毁才终止它。
Owner在宿主边界检查同一scope/core guard，失效即清表；显式close及drop也清表。
回环测试覆盖未读完的闲置HTTP在工具owner销毁、run/Plan检查、close/drop时物理
关闭socket并释放预约。DNS结果只转移宿主私有resolution，没有guest pin入口。
当前120项作者测试（另有需显式组件产物的consumer测试），已接生成的类型化
候选WIT Host和受owner管理的WASI poll。canonical ABI的宿主rep用跨owner不重复的单调ID，
不把可回收的ResourceTable index交给guest；WASI drop同步归还entry账目。
候选clat:net-task的http-start/read-start以owned资源转移确保凭证只用一次、读取
顺序化；结果get仍为类型化私有Resolution/Response/Chunk，没有JSON pin回灌。
生产工具invoke与正式WIT/linker仍未接，不能宣布生产自动清理或D2完成；原生桥作者接线见下。
纯闲置时的失效需要正式host调用边界/生命周期调用close，不能靠无后台任务的
owner自己主动轮询；后续实际WIT/Store所有者必须接入这一点。


作者调度宿主另接最小 WASI clocks、io/streams/error 和 stderr，全部使用同一
Owner，未添加第二张资源表或默认 WASI 网络/文件/环境入口。时钟隐藏根和
pollable 分别计入 16 项上限；订阅失败回滚，pollable 销毁释放隐藏根，Owner
关闭仍按子先父后清理。等待结束再次检查 scope，撤销时在返回 guest 前清表。
非零计时器到期首次 ready 返回 true，零计时器让出调度一次以免饿死 IO。
诊断流仅共享 32KiB 内存缓冲，重开不重置额度，错误转换不分配隐藏 error 资源。
真实作者组件在同一实例连续九次调用，覆盖引擎原生定时器、typed DNS 完成和
取消，每次实际资源表为空；DNS 使用 fake Lookup，仍走原有地址与权限 authority。
这只是调度资源接线验证：net Promise 桥和 AbortSignal 四顺序另由下述原生消费者验证，
生产时钟能力裁决、正式工具生命周期及 D1 总卷仍未完成；正式生命周期需显式关闭 owner。


作者原生 typed bridge 已接到同一 Task：DNS 交付 opaque resolution，HTTP 以 owned
凭证发 HTTP 并交付 opaque response；顺序 read 以 owned response 返回 response 与
最多 64KiB Uint8Array，空块表示真实 EOF。对象使用私有 class/slot、无公开构造器，
不向 guest 暴露数值 handle；HTTP/read 提交前使旧包装失效，预取消保留未转移输入。
资源包装和 chunk 的原型为 null，Promise 交付不调用 ambient Object.prototype.then。
订阅失败释放 task；AbortSignal 拒绝 Promise 同时移除 native AsyncTask，清订阅和
task，不能只发 AbortError。释放 C ABI 列表内存与移交 owned 结果分开处理。

原生 DNS 消费者使用 fake Lookup 和真实 DNS authority/审批；13 次同实例调用覆盖
四顺序、微任务/原生定时器、并行、HTTP fence 拒绝、旧句柄/伪造对象、配额、原型污染
和取消后再次成功，并读取模拟 RFC7050 发现答案及 IPv6 地址族。
HTTP/read 消费者另有 26 次同实例回环调用，使用仅 cfg(test)
可构造的私有 transport credential fixture（0 Lookup），覆盖四顺序、headers/status、
Uint8Array/顺序 read/EOF、输入单次转移、原型污染和 Store 销毁前 socket EOF；新增七种
方法、重复请求头、二进制请求体、提交后的源修改、参数额度、getter 重入/预取消，
以及宿主方法围栏和控制头拒绝。
该回环 fixture 不经过解析器，因此不是 DNS 允许 localhost 或端到端 DNS→HTTP
安全验收。未执行系统 DNS、真实 provider 或官方四件套兼容测试。

作者 API `clatNetRequest(resolution, url, method, headers, body, timeoutMs, maxBytes, signal)`
接收七种大写方法、最多64对请求头/32KiB总字节、最多1MiB Uint8Array；URL沿用
保守ASCII边界，请求头按UTF-8传递，嵌入NUL拒绝。body在读取header前独立复制；
全部转换后复验resource/AbortSignal，防getter重入导致旧凭证再次提交。转换失败保留
输入，宿主拒绝已转移输入则释放；`clatNetHttp` 保留GET快捷方式。
`clatNetHeaders` 返回response头的独立副本，`clatNetAddresses`/`clatNetDns64` 返回
宿主元数据的 `[ip, family]` 副本（family为ipv4/ipv6）；旧info/status接口保留。
这些仍是隔离作者API。生产 invoke 生命周期/时钟能力、v2 能力组合与市场隔离、
D1 24 项 host 总卷、shim/mapping 和原包组件化仍待完成。没有 GC finalizer 中的
host 调用：lost guest wrapper 的资源由配额约束，并需正式工具生命周期 close 回收。
生产 WIT/linker/市场及 Bun/MCP 兜底均未改变。

## 作者能力构造入口与独立时钟声明

隔离作者宿主现在从一个已校验的能力描述符构造 DNS Fence、HTTP Fence 和时钟授权。
该描述符不是已发布的 manifest schema，也没有发行签名或安装复核效力；生产 v1
加载器与索引仍保持原样。候选描述符要求 manifestVersion=2、network protocol为
`clat:net-task@0.1.0`，字段和协议闭集，重复字段/非法类型/无效配置均拒绝。
配置的 origin 与 method 仅同声明求交；显式空 origin 集合拒绝全部。网络声明或
运行配置中出现任何 hostTools/preopens 均拒绝，保守限制也包含只读工具，尚不开放
安全工具例外。sampling 单独声明，出口说明区分受限HTTP/DNS和宿主模型服务；
作者 net linker 本身没有新增 sampling 或 host.call-tool 接口。

时钟不是 network 附赠能力。必须独立声明
`"clock":{"protocol":"wasi:clocks@0.2.10"}`；配置 `"clock":false` 可禁用，
配置不能补授未声明的 clock，也不能更换协议。未声明时 wall/monotonic 的读取、
分辨率和订阅均在分配资源前拒绝；实际 WIT import 不授予权限。声明后仍沿用同一
Owner 的绝对期限、取消、16个实际表项、零计时器让出及订阅子先父后清理。

作者常规卷现131项（另4项显式组件测试）；11项新增覆盖版本/组合/收窄/独立时钟，
其中真实小组件分别验证未声明拒绝、已声明成功。未声明时钟的两个原行为红已保留，
16处能力防线删除均编译成功并产生行为失败。既有39次消费者调用以显式clock声明
复跑通过。以上不替代正式manifest/签名市场隔离、工具生命周期或D1安全总卷。

## 作者网络调用的生命周期边界

隔离网络候选入口只缓存已编译的组件与 linker，每次调用创建新的 Store 和 guest。
它主动关闭同一个资源 Owner 并撤销 scope，覆盖正常返回、guest trap、初始化失败和
宿主 unwind；遗失的 JS opaque wrapper 不依赖 dispose 或 GC finalizer 回收。结束后
原 guest 的 JS 全局状态、canonical handle 表和 native task roots 随 Store 一起销毁。
现有生产 v1 的实例缓存与 Bun/MCP 入口保持原样。

同一 run 跨调用保留累计 DNS/HTTP 尝试预算；新工具不退还 run 额度。入场、返回前及
CPU epoch 都复验原 authority、取消和绝对期限，撤销后不能交付成功结果，也能中断
不再调用宿主的 guest 循环。单 Store 最多一个 memory、256MiB，fuel 仍有上限；
有限 epoch ticker 由候选 lane 持有，lane 销毁时结束并 join。

生命周期常规用例另验证真实回环闲置响应在 Store 存活时 EOF、错误/异常出口、共享
run 预算和 guest 状态隔离；故意丢失 wrapper 的 native consumer 分别正常返回或
抛异常。仍是作者隔离入口，尚未接生产插件工具 invoke；签名 manifest/市场 v2 隔离、
D1 24 项总卷、shim v2 与官方原包组件化继续按后续门禁推进。无系统 DNS 测试。

当前作者常规142项（另5项显式组件用例），生命周期新增11项；8处独立边界删除
均编译后行为红，恢复后全绿。新消费者组件实际执行6次遗失包装/异常场景，并复跑
原DNS13/HTTP26次；共45次通过。完整项目门禁通过，不替代 Windows 真正 CI 或
生产/官方包验收。

### 历史施工记录：D2 作者通道（下述未完成状态仅指当时）

D2、D3 后续已交付过审，生产搜索包与 v2 索引已上线；以下英文段落保留
阶段性验证范围，不表示当前生产安装或 v2 目录仍未实现。

The D2 author lane now derives an explicit-scope flavor without editing the Bun
Shim or official package sources (`sdk/dsh-wasm-flavor`). Its real component
lifecycle oracle passes overlapping injections, asynchronous generator cleanup,
LIFO and failure isolation. The four original packages also componentize through
a closed semantic adapter set. Actual discovery and undeclared-origin denial now pass after fixing Web-standard
compatibility gaps in the pinned engine. The original search provider also sends its POST and reads a real numeric-loopback
test response in the actual component. HTTP-provider pin and cancellation
equivalence remain under verification; componentization therefore does
not establish quartet compatibility or D2 completion. Production installation and the v2 catalog remain unimplemented.


## D3 production integration and search-only release (2026-10-05)

The construction notes above preserve historical status. Manifest v2, the
production loader, signed package storage and https://pi.at.cn/v2/ are now
connected. The v1 linker, original tools/config WIT, released MCP packages and
signed v1 index remain unchanged. The private clat-wasm-net library shares its
source with the audited author backend; core injects the actual permission lease.
Every tool gets a fresh Store and independent deadline, retaining cumulative
active-run attempt budgets, cancellation, deadlines
and active resource closure. No filesystem, preopens or host-tool execution
interface is linked into this network flavor.

The published search-only package exports web_search, with signed POST authority for
https://api.deepseek.com:443 and an independently declared clock. The original
four upstream dependencies remain unchanged. web_fetch is unavailable in both
discovery and invocation; the existing MCP edition retains full search/fetch.
Configuration can only narrow signed network policy. Invalid generations,
protocols, null declarations and conflicting capabilities fail closed. Install,
expansion, rollback and restart use the same validation. Guest errors and traps
are sanitized at the production boundary.

The real component passed ephemeral publisher/index signatures, production
installation, generated-v2-catalog routing, PWA consent, tool/DNS/HTTP approvals,
original search requests/results and uninstall. Numeric loopback routing exists
only in explicitly armed test hosts; ordinary product builds cannot enable it
through environment, guest config or manifests. System DNS is waived on the
owner's TUN machine; production still denies 198.18.* addresses.

Five original-component searches and five Bun searches also passed against real
DeepSeek. The WASM test injects externally obtained A/AAAA answers while retaining
full public-address validation, numeric dialing, TLS and the network authority
chain; it does not claim system DNS acceptance. No relay or fixture reply was used.

The shared HTTP entity layer supports identity, gzip (including concatenated
members), zlib/raw deflate and strict Brotli. Encoded and decoded entities each
have an 8 MiB ceiling, decoded chunks are at most 64 KiB, and entity/decoder
reservations share a 128 MiB process budget. Compressed entities are completely
validated before any decoded bytes are delivered. Authority, cancellation and
deadline checks occur on output chunks and every 4 KiB of compressed input;
unknown or stacked encodings, truncation and bombs fail closed. The six shared
wire vectors pass through the unchanged original provider under Node and the
actual formal WASM tools component. The signed PWA search staging also consumes
a gzip reply. Bun 1.3.14 direct HTTP testing exposes an existing undici Agent
close-method failure; the original fallback is unchanged and this is recorded
separately, not counted as a green Bun encoding comparison. Local acceptance
does not establish independent audit or production publication. Commands
are in [the flavor README](../sdk/dsh-wasm-flavor/README.md); owner signing and
upload are in [publishing](../market/PUBLISHING.md).
