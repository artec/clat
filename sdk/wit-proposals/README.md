# PLG-4 author-only protocol probes

These files are design candidates, not production WIT or an installable plugin.
Use the pinned PLG-3 author dependencies first:

```sh
npm ci --prefix sdk/dsh-wasm-spike --ignore-scripts
node sdk/wit-proposals/validate.mjs
```

The four D1 checks cover legacy, candidate and combined world shapes plus a
missing-dependency negative case. `validate.mjs` parses `net/net.wit` explicitly,
so the adjacent consumer-only world does not widen the contract shape check.

## D2 system DNS and private authority backend

```sh
node sdk/wit-proposals/verify-dns-policy-data.mjs
cargo test --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target --lib dns_authority
cargo fmt --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml -- --check
cargo clippy --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target --all-targets -- -D warnings
node sdk/wit-proposals/check-dns-mutations.mjs /absolute/new-evidence-directory
```

These are separate author checks, not implicitly run by the product full gates.
The 33 tests use independent policy inputs and bounded resolver fixtures. All 21
deletion variants must compile and fail at behavioral assertions; they are copied
to a temporary **differently named Cargo package** so its artifacts cannot replace
the canonical standalone crate's test binary in a shared target directory.

`SystemDns::shared()` is the only production-shaped resolver constructor in this
author library: eight fixed workers, eight queue slots, full OS A/AAAA answers
(reject more than 32), and a fixed `ipv4only.arpa` lookup for IPv6. Workers keep only
a hostname, deadline and weak result slot. Cancelling does not join an uncancellable
OS query; expired results are discarded and a full pool refuses new work. OS-owned
resolver allocation is outside the Rust network-buffer reservation proof.

The host owns run and Store lifetimes. Dropping either owner cancels its outstanding
jobs; borrowed handles cannot extend that lifetime. Runs share the 64 DNS budget
across Stores; each author Scope admits at most 16 live chains, including retained
credentials/pins. This is not yet the final WIT ResourceTable-wide budget. The
bounded author Run has a 120s deadline; production run/tool lifetime integration
still needs to supply the correct per-tool deadline independently of run length.
Origin/config checks happen before queueing. Getters return copies. Credentials
are private, origin/Scope-bound, expire without renewal and consume only once.
There is no HTTP connector, production permission factory or guest pin constructor.

The pinned IANA table conservatively denies all special assignments, including
globally reachable exceptions. Short NAT64 layouts reject nonzero reserved/suffix
bytes. These deliberately narrower candidate policies need compatibility review;
they are not claims of DSH behavior equality.

Optional system DNS checks, **waived on the owner's TUN Fake-IP machine**:

```sh
cargo run --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target --bin dns_probe -- https://example.com
cargo run --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target --bin dns_task_probe -- /absolute/task-consumer.wasm \
  https://example.com --expect-blocked
```

The native probe reuses the accepted scheduling component without changing its WIT
or engine. Its limited failure enum is a probe limitation; policy failure stays
finite and never becomes a success. Five calls reuse one Store: completion,
pre-abort, Promise abort, next call, abort after result. On this machine completion
is `BlockedAddress` because getaddrinfo returns 198.18.*. That rejection and resource
closure are real component evidence, **not public DNS/DNS64 success**. Do not add a
Fake-IP exception or fabricate discovery answers. Actual DNS tests are skipped here
at the owner's instruction. Native timer/parallel-read semantics remain evidenced
by the earlier 21-case scheduling fixture, not this five-call DNS rejection probe.
The complete D1 24-attack matrix, unified HTTP authority chain and shim v2 remain
open; none of the numbers above are a replacement for those gates.

## D2 synchronous cancellation discriminator

```sh
# Refuses an existing directory. Author build environment is empty.
node sdk/wit-proposals/build-sync-abort.mjs /absolute/new-directory

CLAT_PLG4_SYNC_COMPONENT=/absolute/new-directory/sync-abort.wasm \
CLAT_PLG4_REQUIRE_ASYNC=1 \
  cargo test -p clat-core --features test-support \
  plg4_sync_import_cannot_deliver_guest_abort -- --ignored --nocapture
```

**The second command must fail (exit 101) with the current synchronous candidate.**
The real component queues an AbortController Promise continuation before entering
candidate DNS, HTTP headers and body imports. The isolated Wasmtime host holds
each selected import for 50ms; it performs no actual DNS or HTTP I/O. Each phase
prints `enter,return,abort`, followed by the explicit consumer pre-red assertion.
All three traces must be present; a compiler/linker/trap failure is not this proof.
The emitted world has only `clat:net/egress` and `run`; no WASI sockets or ambient
networking are provided. Timers are disabled and not part of this discriminator.

This is evidence that the current waiting protocol blocks guest cancellation,
not a passing asynchronous implementation, a 250ms host-cancellation measurement,
a full upstream lifecycle test, or the 24-case network safety acceptance.
Removing `CLAT_PLG4_REQUIRE_ASYNC` only characterizes the unsuitable synchronous
behavior. Running without `CLAT_PLG4_SYNC_COMPONENT` does no component execution;
an unarmed ignored-test result is not acceptance. This probe is not armed by the
full gates. Component compilation time is not plugin/network operation latency.

The pinned ComponentizeJS 0.23.0 README's Async Support section explains that only
exports are async; custom imports remain synchronous. A Promise wrapper alone
does not change that boundary. No shim v2 or production network mapping has been
implemented. Async design review has now approved isolated dual probes and a
finite DNS/HTTP engine task bridge. The Bun/MCP fallback remains unchanged.

## D2 asynchronous consumer probes

```sh
node sdk/wit-proposals/build-async-probes.mjs /absolute/new-directory
cargo run --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target -- /absolute/new-directory/http-consumer.wasm \
  --require-cancel --reuse-store
```

The builder records three outcomes: native HTTP and the synchronous custom-import
control build; native `async func` imports fail in ComponentizeJS 0.23.0 at
`FunctionKind::AsyncFreestanding => todo!()` (splicer bindgen.rs:721). A missing
build is not a cancellation pre-red. The stock HTTP consumer executes all 15
cases but fails six physical host cancellation assertions, despite returning
AbortError in JavaScript. This is the expected runtime pre-red.

The standalone Rust crate pins Wasmtime/WASI HTTP 48.0.0 and disables
`default-send-request` at compile time. It is outside the CLAT cargo workspace.
Its custom transport never connects or resolves anything: `/dns` labels a delayed
HTTP request-start fixture, **not independent DNS**. Headers and entity reads are
also bounded fixtures. Terminal host events are observed while Store remains
alive; cancellation caused only by Store destruction cannot pass. The matrix
covers no abort, before start, during a Promise continuation, during a native
timer, and after completion. Six additional calls reuse one instance, including
two concurrent body reads followed by cancellation and successful next calls.

## Isolated engine cancellation patch

`engine-patches/close-http.patch` applies to StarlingMonkey commit
`9dda8ba7fcda2e17c6795d402f0478cf4c1f7f37`, the engine pinned by ComponentizeJS
0.23.0 source commit `4d812f5a7b524cea5bcfd565cac55f3fab876a57`.
It drops pending HTTP futures, body input streams and subscriptions, and removes
cancelled tasks from the native event queue. Generic WASI handle behavior is not
changed. No installed engine or production linker is replaced.

In separate fresh source checkouts, apply the patch to the pinned engine, then
configure ComponentizeJS with `STARLINGMONKEY_SRC=/absolute/engine-source`,
`-DCMAKE_BUILD_TYPE=Release -DENABLE_JS_DEBUGGER=OFF`. Build target
`starlingmonkey_embedding` with `RUSTUP_TOOLCHAIN=1.88.0`. The upstream CMake build
requires that author toolchain and may install it/update rustup automatically;
pre-provision and inspect the upstream setup before running. CLAT remains pinned
to Rust 1.98.0. This is an author engine build, not an end-user dependency.

```sh
node sdk/wit-proposals/build-http-engine.mjs /absolute/patched-engine.wasm \
  /absolute/new-patched-directory
cargo run --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target -- /absolute/new-patched-directory/http-consumer.wasm \
  --require-cancel --reuse-store
node sdk/wit-proposals/verify-http-evidence.mjs STOCK_LOG PATCHED_LOG
```

The patched component passes 15 fresh-store and six same-instance cases. The
evidence verifier requires all stock physical-cancellation failures plus all 21
patched outcomes; it only checks captured logs, it does not run components.
The emitted HTTP consumer imports wasi:http types 0.2.10, outgoing-handler
0.2.10, wasi:io and clocks 0.2.12, and stderr 0.2.12 (sink in this host); it imports
no sockets, environment, filesystem or incoming handler. Clocks remain an
isolated probe dependency, not a new production grant.

This proves fixture HTTP cancellation in an actual component/event loop.
Independent DNS/RFC7050, one private resolution-to-connection authority chain,
24 security attacks, shim v2, original quartet compatibility and release
acceptance remain separate gates. Root full gates do not arm these author probes.

## Native DNS/HTTP task scheduling bridge

The approved bridge fallback now has a real C++ `AsyncTask` consumer and real
Wasmtime resource bindings in `task-consumer` and the `task_probe` host binary.
It uses one start/get/subscribe/cancel mechanism for the three closed semantic
phases. This is still a scheduling fixture: there is no resolver, connector,
resolution credential or production ABI. A `dns` job here is **not** a real DNS
answer. This interface must never be registered as production networking.

Use fresh pinned source checkouts (same commits as above). If retaining the
HTTP cancellation patch, apply it first. Pre-provision Rust 1.88.0 with the
wasm32-wasip1 target and CMake; install the C-only generator in an author directory:

```sh
cargo install wit-bindgen-cli --version 0.52.0 --locked \
  --no-default-features --features c --root /absolute/author-tools
node sdk/wit-proposals/prepare-task-engine.mjs /absolute/starling-source \
  /absolute/componentize-source /absolute/author-tools/bin/wit-bindgen
```

The preparer verifies pins, recognized edits, fresh destination files and already
installed tools before writes. It refuses the CLAT checkout and repeated setup.
It also replaces the upstream automatic rustup installer with a preflight check;
this route does not install/update global toolchains. Configure/build the same
`starlingmonkey_embedding` target as above, then:

```sh
node sdk/wit-proposals/build-task-engine.mjs /absolute/native-task-engine.wasm \
  /absolute/new-task-directory
cargo run --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml \
  --locked --target-dir target --bin task_probe -- \
  /absolute/new-task-directory/task-consumer.wasm
```

The host submits immediate jobs and waits only in WASI poll readiness; get never
blocks. Native timers/Promise continuations can cancel the task. The task drops
its pollable before the parent job, closes idempotently, and weak AbortSignal
callbacks cannot revive completed tasks. The matrix is 15 fresh-store plus six
same-instance calls, including two distinct concurrent body jobs. Every return
must leave the host resource table empty while Store is alive. Shared observation
and log-shape helpers are reused with the earlier HTTP probe.

For a discriminating negative, change only the **isolated copied C++ source**:
replace `return ENGINE->cancel_async_task(this);` with `return true;`, rebuild a
separate engine/component and run the same host. All six waiting cancellations
and three sequence cancellations leave resources alive despite JS AbortError;
the host must exit nonzero. Restore by copying the SDK source back, never by
resetting a dirty repository. The restored final component passes 21 cases.

```sh
node sdk/wit-proposals/verify-task-evidence.mjs MUTANT_LOG FINAL_LOG
```

The new component disables HTTP/random and imports only the task interface,
WASI IO, clocks and stderr. The bundled io WIT comes from the pinned
StarlingMonkey `host-apis/wasi-0.2.10/wit/deps/wasi-io-0.2.10/package.wit`;
emitted versions are inspected rather than assumed (current WASI imports 0.2.12).
`expectedNativeSourceSha256` records the SDK source expected by the preparer;
an externally supplied engine still needs source/build provenance and execution.
No production clock capability is granted by this prototype. Real DNS/RFC7050,
typed private resolution ownership through HTTP and all 24 security cases remain
mandatory before shim v2 or unchanged-quartet mapping.

### HTTP 请求权威前置（作者侧，未接 transport）

`http_authority::HttpFence` 只由宿主声明构造：精确 origin 与方法联合判断，
配置只能求交；重复 canonical origin 拒绝。`PreparedRequest` 字段私有、
不可变借用，保留执行数据而审批摘要只含方法和声明 origin，不含路径、
query、headers、body。无 Debug/Serialize 实现，无 permission token 或发送入口。
当前支持七种常用方法（GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS），
扩展方法未支持；控制头和控制字符拒绝，URL 2048B、64头/32KiB、body 1MiB。

```sh
cargo test --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml --target-dir target --lib --locked --offline
node sdk/wit-proposals/check-http-request-mutations.mjs /absolute/new/evidence-directory
```

请求卷9项，加DNS33项共42项；11个独立删除点必须编译成功且对应行为断言红，
当前最后canonical52项复绿（请求9＋DNS33＋连接10）。全部离线，无系统DNS请求。这不是24项安全总卷，
凭证连接、TLS/SNI、生产PermissionFactory/CancelToken、响应实体预算仍待完成。

### 数值地址连接与 TLS（私有作者候选）

`http_authority/connector.rs` 不导出公共连接入口。它将不可变请求origin绑定
到Resolution，单次消费后只把数值SocketAddr交给TCP；域名仅交给TLS ServerName。
TLS使用Rustls默认验证器和WebPKI根，不提供guest信任根、代理、pool或DNS重查。
参考 [Tokio TcpStream接口](https://docs.rs/tokio/latest/tokio/net/struct.TcpStream.html)。
Scope/run失效增加Notify事件；pins.closed先注册通知再查状态，沿用原绝对期限。
连接等待没有后台task，取消会丢弃future和socket；成功后再次检查失效状态。
此处尚无HTTP bytes发送，Host header、redirect、实体预算和生产Network审批尚未接线。

```sh
node sdk/wit-proposals/check-pinned-connect-mutations.mjs /absolute/new/evidence-directory
```

连接10项，与原卷合计52；删除11个独立防线后须出现行为panic（非编译失败），
最后52复绿。测试不调用系统DNS：.invalid原域名＋内部cfg(test)回环pins，
不会放宽生产公网分类。TLS fixture含公开测试key，仅用于本地服务端；默认客户端
必须拒绝此测试CA，可信测试客户端仅用于验证SNI与hostname拒绝，不进入默认根。
直接依赖tokio-rustls/rustls/webpki-roots只进入独立author manifest/lock；core未添加依赖。

### Network/HTTP 作者链（私有）

permission.rs/transport.rs要求注入权威接口；standalone manifest向core单向依赖，
dev-dependency才打开test-support，用opaque fixture/lease/approval适配真实核心
Factory、Plan guard、代际与8worker/8排队执行器。宿主50ms观察同一租约和
CancelToken，非guest/fuel轮询；不在作者库组装第二策略或审批池。
ReadOnly每次询问，ProjectWrite/FullAccess沿用core默认放行；它们均须先经过
请求/地址围栏。权限ABA、服务刷新、clear/new run、parent cancel与工具租约
期限均覆盖header/body等待的物理socket关闭。这些模块不导出发送入口，
尚未注册正式WIT或接入Application的实际工具期限来源。

HTTP/1 driver与fetch同一future驱动，不spawn。原Host/path/query/auth用于真正
请求；审批不复制query/headers/body。3xx直接返回；拒绝upgrade/非identity编码/
响应trailer（保守兼容边界）。64头/32KiB，Content-Length预检与累计body上限8MiB。
body try_reserve_exact固定8MiB，8个实体许可每个按16MiB预约，Response保留许可
直到drop；这是实体buffer预算，不是整个进程RSS测量。原解析deadline贯穿审批/
连接/头/body；取消与失效丢弃driver/socket。参考
[Hyper HTTP/1 Builder](https://docs.rs/hyper/latest/hyper/client/conn/http1/struct.Builder.html)。

```sh
node sdk/wit-proposals/check-network-http-mutations.mjs /absolute/new/evidence-directory
```

作者lib当前88项（DNS/预算49、请求9、连接10、HTTP/审批20）。本轮回环HTTP wire、原Host/
auth、审批secret、redirect、压缩拒绝、长度/累计限制、截断/upgrade、实际mode策略/
逐次审批、拒绝/取消迟到答复、预算预约、body失效关闭、模式变化、头上限均验证。
没有真实DNS、没有公网HTTPS/provider验收，也不是正式24卷。正式
tool deadline来源与64/10预算、WIT ResourceTable与分块read等仍待接线。
旧删除runner在当前revision最终canonical检查已更新88；历史42/52/64日志仍是当时证据。
隔离副本把core path dependency改为真实仓库绝对TOML路径（JSON转义，兼容Windows），
改变临时package身份，只共享依赖缓存，不复制或修改core工作树。

### Core网络审批租约（私有契约，作者HTTP测试已接入）

`src/plugin_host/network.rs`通过实际HostProjectServices的Factory建立策略，
保留Plan ToolAccessGuard。DNS/HTTP均固定Network，审批字段闭集为action、
origin、method；没有原URL、query、header、body或底层错误。
weak bridge、run epoch、权限/服务配置代际、派生CancelToken与绝对deadline
共同封口。权限A→B→A、/new、session恢复、服务配置刷新、clear、新run、
parent cancel、bridge drop、过期及迟到allow均有验证；失败的journal提交不
发布档位或代际。同值档位发布不撤销租约。该模块没有WIT注册或发送入口。

```sh
scripts/gates.sh core_network permission_mode
python3 sdk/wit-proposals/check-core-network-mutations.py /absolute/new/evidence-directory
```

租约/子期限/动态策略11项与执行器5项共16项核心测试、22项独立删除须编译成功并在对应行为断言处失败，最后canonical
16项复绿，包含服务快照与配置刷新交错；临时副本只复制公开
`release/minisign.pub`，不复制签名私钥。
这些测试不发DNS请求。作者88项网络测试经仅test-support构建公开的opaque适配器
消费真实core租约/Factory/审批句柄；这是作者DNS/HTTP接线证据。正式工具
期限来源、64/10预算、WIT ResourceTable/read及完整24卷仍待完成。

`network/approval.rs`私有句柄把实际Factory调用放入进程共享8worker/8队列；
start/get非阻塞，结果只消费一次，drop或失效get取消独立child token。工作项
只持weak结果槽；迟到结果不能恢复失效租约。没有新依赖或WIT入口。作者HTTP
等待已用host异步调度≤50ms观察get/check，不能消耗guest fuel。
无法强行终止不响应cancel的approver，最多占满固定worker并拒新请求；完成后
尚未drop的句柄数量仍须由正式WIT资源预算限制。
删除runner每次使用UUID包身份，隔离canonical及其他临时副本的Rust产物；
只共享依赖cache。固定临时包名导致的旧产物误用日志不计语义红。


### 作者 DNS 前置审批与核心生命周期接线

私有 `http_authority/network.rs` 的 NetworkScope 在准入时锁定期限及资源预约，
Resolve 审批通过后才提交 DNS 池；排队派发、NAT64 发现前、get 和凭证均复用
同一核心租约。弱 scope/slot 工作项不保留完整 run；宿主定时器观察失效，
取消等待与已消费 pins。9项新测试使用 fake Lookup，未发真实 DNS 请求。
真实 ToolAccessSlot revision 使 Plan 开关 ABA、审批期间进入 Plan 和 DNS/HTTP
等待中的策略变更撤销旧授权，不通过配置刷新模拟动态 Plan。

```sh
node sdk/wit-proposals/check-core-dns-binding-mutations.mjs /absolute/new/evidence-directory
```

8项接线删除点以编译成功后的行为断言红验证；旧21项 DNS 删除点在当前源码复验，
canonical 为42项 DNS /75项作者测试。正式 WIT、实际工具期限来源与统一预算、
分块 read 及24项总卷仍待完成。Bun/MCP 原有路径保持不变。


### 共享工具预算与分块响应（作者私有后端）

Tool由host建立，跨Store保留同一截止与10次HTTP尝试计数；run共享64 DNS/64 HTTP，
DNS凭证和HTTP预约共享Store16个逻辑资源上限。取消或drop不返还尝试次数。
NetworkScope.begin_tool 接收host期限；实际生产工具invoke和WIT任务/订阅所有权
尚未连接，不能将逻辑预约计数当成完整ResourceTable限额验收。
StreamResponse先返回状态与头；read为顺序1..65536字节、guest总量上限≤8MiB。
空字节仅表示EOF，失败保持终态；metadata/read复验同一core租约，cancel/drop或
等待read future被销毁时关闭socket和释放实体预约。聚合对照接口调用同一实现。
无分离后台task，不跟随重定向，不扩大编码或网络能力。闲置响应须由正式资源
所有者在run/Store结束销毁；该接线和统一typed任务ABI仍待完成。

```sh
node sdk/wit-proposals/check-stream-budget-mutations.mjs /absolute/new/evidence-directory
```

本轮新增13项测试，作者总88；旧HTTP/DNS删除runner对应当前分块防线更新，
canonical88/49。删除证据不是完整24项总卷；正式WIT与四件套组件化仍未完成。
