# WASM 网络宿主协议提案（PLG-4 D1）

状态：**D1设计审计已通过；D2异步复审已批准，双探针已出证据。**
原生 async import 在锁定作者编译器中不支持；受控 wasi:http 探针经隔离引擎
修复后通过21项取消/连续调用检查。该探针使用无网络的有界传输夹具，未实现真实
DNS、统一授权链或24项宿主安全卷，不能据此宣称四件套兼容或进入映射层。
有限原生任务桥的调度腿也已实跑：DNS/HTTP语义夹具共用任务机制，21项通过；
删去事件循环取消动作时，六处等待取消和三处连续调用取消均暴露残留资源。
它证明独立任务可由原生poll等待，尚未形成携带私有resolution的正式传输接口。
作者宿主现增加真实系统DNS后端、固定8 worker/8排队池、完整地址/NAT64校验、
Store/run失效和私有单次消费凭证。33项纯宿主判别绿，21个独立删除点均以行为
断言失败。真实组件也接入系统DNS等待：本机TUN Fake-IP返回198.18.*，五次连续
调用验证拒绝/取消与资源清空，**没有取得公网DNS或实际DNS64验收**。
负责人明确免做本机真实DNS测试；不为Fake-IP放宽公网规则。
下述作者接线继续补齐核心审批与 HTTP；正式 WIT 入口、统一预算与完整24项
攻击卷仍待完成；以上33项不是那份24项总卷。
作者侧 HTTP 请求前置现增加精确 origin+method 围栏、配置求交、不可变请求对象、
URL/头/body 预算与控制头拒绝；9项请求卷通过（加原DNS卷共42项）。审批摘要
只输出方法和声明origin，不输出路径/query/header/body；尚无发送或授权入口。
候选方法暂只支持 GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS，扩展方法需后续
兼容复审；这些请求校验不能替代生产审批、TLS或完整24项攻击卷。
作者候选现已将请求origin与私有凭证绑定，单次消费后以数值SocketAddr连接，
原域名用于TLS SNI/证书身份校验；默认WebPKI根，无代理或再次DNS解析路径。
10项离线/回环连接测试通过（作者lib合计52项）：真实TCP/TLS握手、正确SNI、
错误域名/不可信证书拒绝、取消后物理socket关闭、原期限与迟到成功拒绝。
11个连接防线删除点均行为红。该connector保持私有，由下述作者HTTP后端调用；
正式WIT入口与统一预算尚未完成，不能开放到生产linker。
回环证书与私钥仅为公开测试夹具，不能用作生产信任根。
作者HTTP后端现要求注入权威接口；测试构建通过core test-support的opaque适配器
消费真实PermissionFactory/Plan guard、租约与8worker/8排队审批器。等待用宿主
50ms定时检查同一租约/CancelToken（不消耗guest fuel）。私有HTTP/1候选已发送真实回环请求并读取响应：原Host/target/auth保持，
审批仅安全摘要；3xx直接返回，identity实体≤8MiB，其他编码明确拒绝；64响应头、
32KiB头缓冲、8个16MiB实体预约许可，响应保留许可直到drop。driver与读取同一
future驱动，无分离后台任务。当前作者lib88项通过；不是24项安全总卷。
core租约的权限ABA、服务刷新、clear/new run、父取消及期限失效已验证在header/body
等待期间物理关闭socket；Resolve/HTTP动作均经过真实Factory与Plan guard。
**仍未接正式WIT联网入口、正式tool期限来源、
实际工具调用边界、任务/订阅与WIT资源表统一核算**；上述模块私有，不能开放生产linker。
作者响应现支持状态/头先返回与有界分块read，尚未接正式WIT资源接口；只实测回环HTTP及独立TLS
握手，未做公网provider/四件套兼容验收。
下文保留已审策略；同步 WIT 仅为淘汰候选，异步等待机制须承载同一策略。
候选 [WIT 全文](../sdk/wit-proposals/net/net.wit) 位于 author SDK，
不进入现有 `wit/plugin.wit` 或生产 linker。Bun/MCP 配方和正式市场数据不变。

## 决策与适用范围

选择独立 `clat:net/egress@0.1.0`，保留 `clat:plugin@0.1.0`。
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
作者网络库现88项测试，已通过测试适配器消费真实租约与审批句柄。
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
