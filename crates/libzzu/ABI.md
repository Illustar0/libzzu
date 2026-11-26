# libzzu C ABI 规范

本文档定义 `libzzu` 与各语言 SDK 之间的二进制接口及 JSON 通信协议。C 类型与函数签名以 [`include/libzzu.h`](include/libzzu.h) 为准；本文档规定设计约束、内存与线程契约、数据编码格式以及全部客户端的方法目录。

---

## 架构与版本契约

### 句柄拓扑与符号设计

`libzzu` 的不透明句柄与 `zzu` 中的领域客户端一一对应，不合并为单一聚合客户端。各服务的认证前提与生命周期彼此独立：本科教务（`ZzuEasClient`）、校园卡（`ZzuEcardClient`）与校园邮箱（`ZzuMailClient`）派生自统一身份认证客户端（`ZzuCasClient`），共享其认证会话、后台运行时与操作时限；校园网 Portal（`ZzuPortalClient`）与自助服务（`ZzuSelfServiceClient`）则直接挂载在运行时（`ZzuRuntime`）下，不依赖 CAS 账号。上层 SDK 封装应保持这一类型结构。

```text
ZzuRuntime ─┬─ zzu_cas_new ──────────┬─ zzu_eas_new
            │                        ├─ zzu_ecard_new
            │                        └─ zzu_mail_new
            ├─ zzu_portal_new
            ├─ zzu_self_service_new
            └─ zzu_portal_discover / zzu_portal_discover_blocking
```

整套 ABI 共导出 38 个 C 函数（Android 平台另导出 1 个 JNI 初始化符号），遵循以下边界约束：

- **零结构体跨界**：所有导出函数的签名仅使用 `int32_t`、`uint32_t`、`size_t`、指针与单一回调类型 `ZzuCallback`。不传递结构体值，也不暴露结构体内存布局或 C `bool`（避免 .NET 默认 4 字节布尔封送及跨平台对齐差异）。
- **定宽状态码**：`ZzuStatus` 定义为 `int32_t` 类型别名而非 C `enum`，消除编译器枚举底层宽度差异，并允许次版本新增状态码而不触发跨语言枚举反序列化未定义行为。
- **方法名分发 I/O，独立导出纯函数**：每种客户端固定导出 `new`、`free`、`call`、`call_blocking` 四个函数，客户端状态查询与网络操作统一经 `call` 以“方法名 + UTF-8 JSON”分发；当动态库版本旧于 SDK 时，仅新方法调用返回 `ZZU_UNKNOWN_METHOD`，不会在动态链接阶段因缺失符号导致整个库加载失败。不依赖客户端状态的本地纯计算（如课表转日历、房间号校验）单独导出为顶层 C 函数，避免引入多余的句柄或登录依赖。

| 类别 | 导出函数 | 说明 |
| --- | --- | --- |
| 版本查询 | `zzu_abi_version`、`zzu_version` | 查询 C ABI 版本号与库软件版本号 |
| 缓冲区 | `zzu_buffer_data`、`zzu_buffer_len`、`zzu_buffer_free` | 读取与释放本库分配的字节缓冲区 |
| 运行时 | `zzu_runtime_new`、`zzu_runtime_free` | 创建与释放后台异步运行时（通常每进程一个） |
| CAS 客户端 | `zzu_cas_new`、`zzu_cas_free`、`zzu_cas_call`、`zzu_cas_call_blocking` | 统一身份认证、MFA 验证、令牌读写、个人信息与业务系统单点登录 |
| 教务客户端 | `zzu_eas_new`、`zzu_eas_free`、`zzu_eas_call`、`zzu_eas_call_blocking` | 本科教务登录、学期校历、学期课表与教室占用查询 |
| 校园卡客户端 | `zzu_ecard_new`、`zzu_ecard_free`、`zzu_ecard_call`、`zzu_ecard_call_blocking` | 校园卡余额、在线校园码、账单流水、宿舍电费查询与充值、校园卡充值下单 |
| 校园邮箱客户端 | `zzu_mail_new`、`zzu_mail_free`、`zzu_mail_call`、`zzu_mail_call_blocking` | 校园邮箱会话登录、网页版入口地址与收件箱未读邮件数查询 |
| Portal 探测 | `zzu_portal_discover`、`zzu_portal_discover_blocking` | 自动探测当前网络下的校园网 Portal 地址与本机内网 IP |
| Portal 客户端 | `zzu_portal_new`、`zzu_portal_free`、`zzu_portal_call`、`zzu_portal_call_blocking` | 校园网 `eportal` 终端上线认证 |
| 自助服务客户端 | `zzu_self_service_new`、`zzu_self_service_free`、`zzu_self_service_call`、`zzu_self_service_call_blocking` | 校园网在线终端列表查询与强制下线 |
| 调用控制 | `zzu_call_cancel`、`zzu_call_free` | 请求取消在途异步调用、释放调用句柄 |
| 本地纯函数 | `zzu_timetable_to_calendar`、`zzu_room_id_parse`、`zzu_recharge_order_checkout_url_for` | 无需运行时与客户端的同步数据转换与校验 |

### 版本号与演进规则

`zzu_abi_version()` 返回 `uint32_t`，编码格式为 `(major << 16) | minor`。SDK 加载动态库时须校验版本兼容性：

```c
uint32_t version = zzu_abi_version();
uint32_t major = version >> 16;
uint32_t minor = version & 0xFFFF;
// 要求 major == ZZU_ABI_VERSION_MAJOR 且 minor >= ZZU_ABI_VERSION_MINOR
```

- **递增次版本号（`ZZU_ABI_VERSION_MINOR`）**：新增客户端 JSON 方法、新增可选请求字段、在响应对象中新增字段、新增 `ZzuStatus` 状态码或错误 `kind` 取值，以及新增顶层导出函数。旧版 SDK 无需重编译即可链接新版动态库。
- **递增主版本号（`ZZU_ABI_VERSION_MAJOR`，同时次版本号归零）**：修改或删除既有导出函数签名、删除或重命名既有方法与字段、将可选请求字段改为必填，或改变既有字段的语义与类型。

`zzu_version()` 返回静态存储的 `NUL` 结尾软件版本字符串（如 `"0.1.0"`），进程生命周期内始终有效，调用方**不得**释放该指针。

---

## 内存与调用约定

### 所有权与生命周期

跨语言混用内存分配器会破坏堆结构，所有跨越 ABI 边界的指针遵循“谁分配、谁释放”原则：

- **输入参数同步复制**：方法名 `method` 为 `NUL` 结尾的 UTF-8 字符串；字节切片参数（`config`、`options`、`params`、`room`）为 UTF-8 字节序列，**不要求**以 `NUL` 结尾。当长度参数为 `0` 时，对应数据指针可传 `NULL`；长度大于 `0` 时数据指针须非空且在调用期间可读。所有函数（含异步 `zzu_*_call`）均在返回前完成输入复制，调用方在函数返回后即可立即释放或复用输入缓冲区。
- **输出缓冲区（`ZzuBuffer`）**：由本库分配，承载成功结果或错误详情的 UTF-8 JSON 字节流。调用方通过 `zzu_buffer_data` 获取首字节地址、通过 `zzu_buffer_len` 获取字节长度，读取完毕后**必须**调用 `zzu_buffer_free` 释放。缓冲区末尾**不保证**含 `NUL` 字节，严禁直接当作 C 字符串传入 `strlen` 等函数。向 `zzu_buffer_data` 传入 `NULL` 返回 `NULL`，向 `zzu_buffer_len` 传入 `NULL` 返回 `0`。
- **回调上下文（`user_data`）**：由本库原样透传给 `ZzuCallback`，可为 `NULL`。若非空，调用方须保证该指针可安全跨线程传递，并在回调触发前保持有效。
- **释放函数幂等（`zzu_*_free`）**：所有 `zzu_*_free` 函数传入 `NULL` 时均为空操作，不阻塞当前线程，可在任意线程（包括回调内部）调用。每个非空句柄只能释放一次；释放 `ZzuBuffer` 后，此前由 `zzu_buffer_data` 取得的数据指针立即失效。

### 四类调用模式

#### 1. 构造与释放（`zzu_*_new` / `zzu_*_free`）

所有构造函数均在本地同步完成，不发起网络请求。

- `out_*` 句柄输出槽位**必须**非空（传 `NULL` 返回 `ZZU_INVALID_INPUT`）。函数入口处会先将 `*out_*` 置为 `NULL`；返回 `ZZU_OK` 时写入有效句柄，失败时保持 `NULL`。
- `out_error` 错误输出槽位可传 `NULL`。若非空，成功时置为 `NULL`，失败时写入含错误 JSON 的 `ZzuBuffer *`（须通过 `zzu_buffer_free` 释放）。
- 运行时、客户端会话与在途任务之间通过原子引用计数维护生命周期，各句柄可按**任意顺序**释放：释放 `ZzuCasClient` 不影响已派生的 `ZzuEasClient`、`ZzuEcardClient` 与 `ZzuMailClient`；释放客户端或 `ZzuRuntime` 不会中断在途调用，底层工作线程池会在最后一个客户端句柄与最后一个在途任务均结束后自动在后台关闭。

#### 2. 异步调用（`zzu_*_call`、`zzu_portal_discover`）

异步接口是推荐的主调用路径，适合拥有事件循环、协程或异步运行时的宿主语言。函数提交任务后立即返回，绝不在当前调用栈内同步触发回调。

- **两阶段错误边界**：
  - **提交阶段（同步返回值）**：仅校验调用入参（客户端或运行时句柄非空、`method` 非空且为合法 UTF-8、`callback` 非空、`len > 0` 时数据指针非空；`zzu_portal_discover` 还会在此阶段校验 `options` JSON）。校验失败时直接返回 `ZZU_INVALID_INPUT`，此时 `*out_call`（若非空）为 `NULL`，**回调绝不会触发**。
  - **执行阶段（异步回调）**：只要函数返回 `ZZU_OK`，`callback(user_data, status, payload)` **必定在后台工作线程上恰好触发一次**——无论是正常完成、`params` JSON 解析失败、方法名未知、网络错误、超时、取消、Rust 内部 panic，还是提交后立即释放了客户端与运行时句柄。
- **输出时序保证**：`out_call` 可传 `NULL`（无需取消时）。若非空，`*out_call` 保证在任务派发前写入，因此即使后台线程在 `zzu_*_call` 返回前就触发了回调，`*out_call` 也已处于有效状态。该句柄须通过 `zzu_call_free` 释放。
- **回调载荷所有权**：回调收到的 `payload` 恒非空（`status == ZZU_OK` 时为结果 JSON，否则为错误 JSON），所有权完整移交给回调方。回调方既可当场读取并调用 `zzu_buffer_free(payload)`，也可将 `payload` 指针投递到其他线程（如 Dart `NativeCallable.listener`）延后读取与释放。

#### 3. 阻塞调用（`zzu_*_call_blocking`、`zzu_portal_discover_blocking`）

为无法接收跨线程回调的语言或脚本场景提供，阻塞当前线程直至操作完成或达到 `timeoutMs` 时限。该模式不支持中途取消。

- `out_payload` **必须**非空（传 `NULL` 直接返回 `ZZU_INVALID_INPUT`）。非空时无论成功或失败均会写入非空的 `ZzuBuffer *`（成功为结果 JSON，失败为错误 JSON），须调用 `zzu_buffer_free` 释放。
- **严禁在回调线程中调用**：在 `ZzuCallback` 内调用任何 `*_blocking` 函数会立即返回 `ZZU_INVALID_INPUT`（避免耗尽后台工作线程导致死锁）。

#### 4. 本地数据纯函数

`zzu_timetable_to_calendar`、`zzu_room_id_parse`、`zzu_recharge_order_checkout_url_for` 不依赖 `ZzuRuntime` 与任何客户端句柄，不发起网络请求，直接在当前调用线程同步计算完成，可在任意线程（包括 `ZzuCallback` 内部）安全调用。`out_payload` 契约与阻塞调用一致。

### C 调用流程示例

```c
ZzuRuntime *runtime = NULL;
ZzuCasClient *cas = NULL;
ZzuEasClient *eas = NULL;
ZzuBuffer *error = NULL;
ZzuCall *call = NULL;

// 1. 初始化运行时与客户端（本地完成，无网络请求）
// cfg 是包含 account、password 和当前设备 deviceId 的 JSON 字符串。
zzu_runtime_new(NULL, 0, &runtime, NULL);
zzu_cas_new(runtime, (const uint8_t *)cfg, strlen(cfg), &cas, &error);
zzu_eas_new(cas, &eas, &error);

// 2. 依次完成 CAS 与教务登录（异步调用须等待前一步回调成功后再发起下一步依赖调用）
if (zzu_cas_call(cas, "login", NULL, 0, on_cas_login, ctx, &call) == ZZU_OK) {
    zzu_call_free(call); // 释放取消句柄不会中断在途调用
}

// 3. on_cas_login 成功后调用 zzu_eas_call(eas, "login", ...)；教务登录完成后可并发发起多项只读查询
zzu_eas_call(eas, "semesters", NULL, 0, on_complete, ctx_semesters, NULL);
zzu_eas_call(eas, "timetable", NULL, 0, on_complete, ctx_timetable, NULL);

// 4. 按任意顺序释放客户端与运行时
zzu_eas_free(eas);
zzu_cas_free(cas);
zzu_runtime_free(runtime);
```

---

## 并发、时限与取消

- **线程安全与读写并发**：所有导出函数均支持多线程并发调用。各客户端按状态可变性采用不同的内部同步策略：
  - **公平异步读写锁（`ZzuEasClient`、`ZzuEcardClient`、`ZzuMailClient`、`ZzuSelfServiceClient`）**：内部使用 Tokio 写优先（FIFO 队列）异步读写锁保护客户端状态。读操作持共享读锁并发执行；写操作持独占写锁，等待已在途的读操作全部完成后执行，并在排队与执行期间阻塞后续新到达的读操作，既防止写饥饿，又保证登录、登出等会话状态切换的一致性。注意：异步任务由多线程工作池调度，调用提交顺序不等同于抢锁顺序，存在状态先后依赖的操作（如先 `login` 后查询）仍须等待前一步回调完成后再发起：
    - `ZzuEasClient`：`login`、`logout` 独占写锁；其余查询方法（`is_logged_in`、`current_semester`、`semesters`、`timetable`、`classroom_catalog`、`classroom_occupancies_page`、`classroom_occupancies`）共享读锁并发执行。
    - `ZzuEcardClient`：`login`、`logout`、`recharge_energy`、`create_recharge_order` 独占写锁；其余查询与收银台方法（`is_logged_in`、`default_room`、`balance`、`payment_code`、`transactions`、`locations`、`remaining_energy`、`recharge_config`、`payment_channels`、`prepare_payment`）共享读锁并发执行（内部仅在读写会话、默认房间与校园码刷新配置缓存时短暂持状态锁，释放锁后再发网络请求）。
    - `ZzuMailClient`：`login`、`logout` 独占写锁；`is_logged_in`、`webmail_url`、`unread_count` 共享读锁并发执行（内部仅在读写会话缓存时短暂持状态锁，释放锁后再发网络请求）。
    - `ZzuSelfServiceClient`：`login`、`logout` 独占写锁；`is_logged_in`、`online_devices`、`kick_device` 共享读锁并发执行。
  - **会话互斥锁（`ZzuCasClient`）**：认证与 MFA 状态变更（及派生的教务、校园卡、邮箱客户端读取令牌）受同一异步会话互斥锁保护；`user_info` 与 `service_sso_url` 在读取令牌并释放会话锁后并发执行网络请求。
  - **无状态全并发（`ZzuPortalClient`）**：无可变会话状态，所有调用完全并发执行。
- **回调执行约束**：`ZzuCallback` 运行在名为 `zzu-worker` 的后台工作线程上，须尽快返回，以免阻塞运行时调度。回调必须使用 C 调用约定，**严禁**让 C++ 异常、托管异常或 Rust panic 越过 FFI 边界向外栈展开。回调内除不得调用 `*_blocking` 函数外，可自由调用 `zzu_buffer_*`、`zzu_*_free`、`zzu_*_call` 及本地纯函数。
- **单次操作总时限（`timeoutMs`）**：客户端与 `zzu_portal_discover` 配置中的 `timeoutMs` 为单次操作端到端总时限（含等待客户端读写锁、会话锁或令牌换新锁的排队耗时及内部多次 HTTP 往返），默认值为 `15000` 毫秒，须为正整数（传 `0` 返回 `ZZU_INVALID_INPUT`）。超时后操作中止并返回 `ZZU_TIMEOUT`。库内不设其他隐式请求超时。
- **尽力而为取消（`zzu_call_cancel`）**：可在任意线程对有效 `ZzuCall` 重复调用；若操作已结束则为空操作。调用取消后，异步回调依然保证触发一次，状态码通常为 `ZZU_CANCELLED`，若完成与取消发生竞态也可能收到真实执行结果。注意：调用 `zzu_call_free` 仅释放句柄内存，**不会**取消操作。对 `recharge_energy` 等非幂等写操作发起取消或遇到超时后，服务端可能已受理扣款，应用层须先查询余额核实状态，切勿盲目自动重试。
- **Panic 隔离与服务损坏**：本库在所有 FFI 入口与异步任务边界捕获 Rust panic 并转换为 `ZZU_PANIC`，绝不向宿主进程展开栈。若 `ZzuEasClient`、`ZzuEcardClient`、`ZzuMailClient` 或 `ZzuSelfServiceClient` 在持有内部读写锁（无论共享读锁还是独占写锁）期间发生 panic，该客户端实例会标记为已损坏（poisoned），后续一切读写调用均直接返回 `ZZU_PANIC`，调用方须释放该句柄并重新创建。
- **`fork` 限制**：`ZzuRuntime` 内部持有操作系统工作线程池与 I/O 轮询句柄，不具备 `fork` 安全性。使用 `fork` 创建子进程时，须在 `fork` 之后再加载并初始化 `ZzuRuntime`。

---

## 状态码与数据格式

### 状态码一览（`ZzuStatus`）

本库不使用线程局部的 `last_error`（协程与 goroutine 跨操作系统线程迁移会导致线程局部状态错乱），所有错误均通过 `ZzuStatus` 返回值与对应的错误 JSON 缓冲区成对传递。封装层遇到表中未列出的状态码时，应统一归入 `ZZU_UNKNOWN_ERROR` 处理。

| 常量 | 数值 | 含义与处理建议 |
| --- | --- | --- |
| `ZZU_OK` | `0` | 操作成功完成，载荷为结果 JSON |
| `ZZU_INVALID_INPUT` | `1` | 空指针、非 UTF-8 文本、非法 JSON、未知参数字段、参数取值越界，或在回调线程中调用了 `*_blocking` |
| `ZZU_TRANSPORT_ERROR` | `2` | 网络连接建立失败或底层传输读写异常（细分原因见错误 JSON 的 `kind`） |
| `ZZU_HTTP_ERROR` | `3` | 服务端返回非 2xx 的 HTTP 状态码 |
| `ZZU_DECODE_ERROR` | `4` | 服务端响应报文、HTML、JSONP 或协议字段解析校验失败 |
| `ZZU_AUTHENTICATION_ERROR` | `5` | 未登录、凭据错误、令牌过期或多因素认证未通过（细分原因见错误 JSON 的 `kind`） |
| `ZZU_REMOTE_ERROR` | `6` | 远端服务正常响应，但返回业务级拒绝或失败结果 |
| `ZZU_NOT_FOUND` | `7` | 请求的目标资源不存在（如指定的 `semesterId` 不在学期列表中） |
| `ZZU_CRYPTO_ERROR` | `8` | RSA、SM2 或 SM4 密钥解析与加解密运算失败 |
| `ZZU_RUNTIME_ERROR` | `9` | 内部异步运行时初始化或线程通道通信失败 |
| `ZZU_PANIC` | `10` | Rust 内部发生 panic，或客户端先前已因 panic 损坏；须释放该客户端句柄并重建 |
| `ZZU_UNKNOWN_ERROR` | `11` | 未归类的其他错误 |
| `ZZU_TIMEOUT` | `12` | 单次操作（含排队）超过 `timeoutMs` 总时限 |
| `ZZU_CANCELLED` | `13` | 操作已由 `zzu_call_cancel` 取消 |
| `ZZU_UNKNOWN_METHOD` | `14` | 当前链接库不支持该方法名（通常代表 SDK 版本新于动态库文件） |

### 错误 JSON 结构

当回调状态码或同步返回值为非 `ZZU_OK`（且存在输出缓冲区）时，载荷统一为以下 JSON 对象：

```json
{
  "status": 5,
  "kind": "mfa_required",
  "message": "需要完成 MFA 验证",
  "cause": "底层错误原因链，无底层源时为 null",
  "request": {
    "method": "POST",
    "url": "https://cas.s.zzu.edu.cn/token/password/passwordLogin",
    "statusCode": 401,
    "errorCode": "业务错误码文本或 null",
    "responseBody": "原始响应体文本或 null"
  }
}
```

- `kind`：错误细分标识（`string | null`）。封装层须容忍未来次版本新增的字符串取值：
  - `ZZU_TRANSPORT_ERROR` 下可能为 `"timeout"`（底层传输超时）、`"connection"`（连接失败）或 `null`。
  - `ZZU_AUTHENTICATION_ERROR` 下可能为 `"invalid_credentials"`（账号或密码错误）、`"not_authenticated"`（尚未登录或已登出）、`"token_expired"`（本地判定 CAS 令牌过期或即将过期）、`"mfa_required"`（需完成多因素认证）、`"invalid_mfa_state"`（MFA 调用顺序或前置状态不满足）、`"mfa_verification_failed"`（短信验证码校验未通过）或 `null`。
  - 其他状态码下固定为 `null`。
- `request`：关联的 HTTP 诊断上下文，非网络请求产生的错误下为 `null`。`url` 已剥离查询参数、片段与用户凭据；但 `responseBody` 可能包含服务端返回的个人敏感信息，**SDK 默认不得将 `responseBody` 输出到日志**。

### JSON 编码规范

- **命名风格**：所有配置、请求参数与响应对象的字段名均采用 `camelCase`，枚举字符串采用 `snake_case`。无返回值的方法成功时返回 JSON 字面量 `null`。
- **空参数传递**：无参方法或全部字段均取默认值的配置/参数，传入 `NULL` 与长度 `0`（内部等价于 `{}`）或传入 `"{}"` 均可；**不得**传入字面量 `"null"`（参数反序列化期望 JSON 对象，传 `"null"` 会触发 `ZZU_INVALID_INPUT`）。
- **严进宽出**：
  - **请求与配置拒绝未知字段**：所有带参方法与配置对象默认开启未知字段校验，字段名拼写错误（如误传 `"semester_id"`）会立即返回 `ZZU_INVALID_INPUT`，绝不静默忽略而退回默认值。仅三处例外：无参方法不解析 `params`；`ZzuCasClient` 的 `set_tokens` 忽略多余字段并兼容 `"idToken"` 别名；校园卡支付相关方法的 `order` 字段允许携带 `create_recharge_order` 返回对象中的 `id` 与内部扩展字段。
  - **响应须忽略未知字段**：次版本升级可能在任意响应对象中新增字段，SDK 反序列化时**必须**忽略未知属性。
- **金额精度**：查询返回的货币金额（如 `balance`）一律编码为精确的十进制字符串（如 `"12.34"`），SDK 须映射为高精度十进制类型（如 `Decimal`、`BigDecimal`），**不得**转换为二进制浮点数。充值参数与面额配置中的 `amount`、`amounts` 单位都是元，不是分，取值为正整数（`uint32`）。
- **日期、时刻与带时区时间**：
  - 公历日期采用 `YYYY-MM-DD` 字符串（如 `"2026-10-26"`）。
  - 不绑定日期的每日时刻（如作息节次起止时间）采用 ISO 8601 `HH:MM:SS` 字符串（如 `"08:00:00"`，反序列化同时兼容 `"HH:MM"`），小数秒按需输出，最多 9 位。
  - 带时区时间采用 RFC 9557 格式字符串（如 `"2026-10-26T08:00:00+08:00[Asia/Shanghai]"`），小数秒按需输出，最多 9 位（纳秒）；尽管当前课表时间精确到分、在线设备登录时间精确到秒，SDK 解析器仍不应假定不含小数秒。

---

## 会话与令牌管理

本库将协议状态机、令牌可用性校验与并发同步留在原生层，将持久化存储与重登时机完全交由 SDK 与上层应用控制：

| 原生层（`libzzu`）职责 | SDK 或应用层职责 |
| --- | --- |
| CAS 密码加密登录、短信 MFA 状态机与读写并发同步 | 选择安全凭据存储（Keychain、Keystore 等）持久化令牌对 |
| 校验 JWT 结构与过期时间，在令牌不可用时返回 `token_expired` | 应用启动时恢复令牌，决定何时触发重新登录或弹出 MFA 界面 |
| 校园卡 `access_token` 与校园邮箱 `sid` 过期或 CAS 令牌变更时按需合并换新 | 处理非幂等操作异常后的余额核实与用户提示 |

- **无隐式后台行为**：本库不启动任何后台保活或定时刷新任务，也**绝不会**在业务调用遇到令牌失效时隐式使用缓存密码重登——密码重登可能挤掉用户在手机端官方 App 的登录态，必须由应用层显式调用 `login` 或 `reauthenticate` 触发。
- **令牌持久化与恢复**：CAS 令牌仅在调用 `login`、`reauthenticate`、`set_tokens`、`logout` 时发生变更；应用可在这些调用完成后读取 `tokens` 并存入系统安全存储。进程重启恢复会话时，依次调用 `set_tokens` 与 `login`：只要预置令牌仍在可复用有效期内，`login` 会直接激活本地登录态，不发起任何网络请求。
- **`token_expired` 判定规则**：已登录会话中，当 `userToken` 剩余有效期不足 15 分钟、`refreshToken` 已过期，或无法从 JWT 解析过期时间时，`user_token` 及所有依赖它的教务、校园卡、校园邮箱、个人信息与业务系统单点登录方法均返回 `ZZU_AUTHENTICATION_ERROR`（`kind: "token_expired"`）。该检查纯属本地时间校验；若服务端提前吊销了未过期的令牌，远端接口会返回认证或业务错误。重新调用 `login` 时仍可能触发 `mfa_required`，处理流程与首次登录一致。
- **校园卡会话换新与并发合并**：校园卡客户端完成 `login` 后，在每次业务请求前短暂读取缓存校验内部 `access_token`，拿到可用令牌后立即释放状态锁再发起业务请求。若距上次换取已满 45 分钟（经验阈值）或底层 CAS `userToken` 已更新，内部通过独立的异步换新锁将并发换新合并为单次令牌交换（不消耗密码），多个等待中的请求直接复用同一次换新结果；若换新请求中途超时或被取消，换新锁自动释放，后续请求可继续正常触发换新。若此时底层 CAS 令牌本身已不可用，则不会换新，即使校园卡会话未满 45 分钟也直接返回 `token_expired`。
- **校园邮箱会话换新与并发合并**：校园邮箱客户端完成 `login` 后，缓存网页版入口地址与 `sid`。每次调用 `webmail_url` 或 `unread_count` 前会校验底层 CAS `userToken` 与缓存新鲜度（20 分钟经验阈值）；若会话超期、底层 CAS `userToken` 已更新，或 `unread_count` 收到服务端会话失效响应（`FA_INVALID_SESSION`、`FA_INVALID_TEMP_SESSION` 或 `FA_SECURITY`），内部通过异步换新锁合并换新会话（不消耗密码）并自动重试一次查询。若底层 CAS 令牌已不可用，直接返回 `token_expired`。

---

## 初始化配置

除 `zzu_eas_new`、`zzu_ecard_new` 与 `zzu_mail_new` 直接沿用 `ZzuCasClient` 的运行时和 `timeoutMs` 外，其余构造函数及 Portal 探测函数均接收 UTF-8 JSON 配置对象（字段名后带 `?` 表示可选）：

| 函数 | 配置 JSON 结构 | 字段要求与默认值 |
| --- | --- | --- |
| `zzu_runtime_new` | `{"workerThreads?": 2, "rootCertificatesPem?": "..."}` | 可传 `NULL` 与长度 `0`（Android 除外，见“Android 平台初始化”）；`workerThreads` 为正整数，默认 `2`；`rootCertificatesPem` 为 PEM 格式的根证书，提供时只信任它们 |
| `zzu_cas_new` | `{"account": "...", "password": "...", "deviceId": "...", "timeoutMs?": 15000}` | `account`、`password`、`deviceId` 为必填字符串。`deviceId` 必须含有非空白字符，缺失或无效时返回 `ZZU_INVALID_INPUT`。`timeoutMs` 为正整数毫秒，默认 `15000` |
| `zzu_eas_new` | 无配置参数 | 沿用所传入 `ZzuCasClient` 的运行时、CAS 会话与 `timeoutMs` |
| `zzu_ecard_new` | 无配置参数 | 沿用所传入 `ZzuCasClient` 的运行时、CAS 会话与 `timeoutMs` |
| `zzu_mail_new` | 无配置参数 | 沿用所传入 `ZzuCasClient` 的运行时、CAS 会话与 `timeoutMs` |
| `zzu_portal_discover` / `_blocking` | `{"timeoutMs?": 15000}` | 可传 `NULL` 与长度 `0`；`timeoutMs` 为正整数毫秒，默认 `15000` |
| `zzu_portal_new` | `{"baseUrl": "...", "userIp": "...", "sourceAddress?": "user_ip", "timeoutMs?": 15000}` | `baseUrl` 为含主机名的 `http`/`https` URL（通常取探测结果的 `portalServerUrl`）；`userIp` 为门户识别的终端 IP（通常取探测结果的 `userIp`，登录时作为终端地址发送）；`sourceAddress` 为 `"user_ip"`（默认，把 `userIp` 绑定为本地源地址，适用于终端直接接入校园网）或 `"system"`（不绑定，由系统路由选择源地址，适用于终端位于 NAT 路由器后、`userIp` 为路由器 WAN 口地址）；`timeoutMs` 默认 `15000` |
| `zzu_self_service_new` | `{"baseUrl": "...", "timeoutMs?": 15000}` | `baseUrl` 为校园网自助服务根地址（`http`/`https` URL）；`timeoutMs` 默认 `15000` |

---

## 客户端方法目录

以下方法按客户端句柄分类，通过对应的 `zzu_*_call` 或 `zzu_*_call_blocking` 调用。参数列标为“无”表示无需传参（传 `NULL` 与长度 `0`，或传 `"{}"`）；字段名带 `?` 表示该字段可省略。

### `ZzuCasClient`（统一身份认证）

`deviceId` 可以是任意包含非空白字符的字符串，但它本质上是凭证，请确保它唯一且难以预测。
密码登录和 MFA 检测都使用创建客户端时传入的 `deviceId`。

| 方法名 | 参数 JSON 字段 | 结果 JSON 类型 | 行为说明 |
| --- | --- | --- | --- |
| `login` | 无 | `null` | 登录 CAS 会话；已缓存可复用令牌时直接激活，不发起网络请求。若需多因素认证，返回 `ZZU_AUTHENTICATION_ERROR`（`kind: "mfa_required"`）。可先调用 `prepare_sms` 预览脱敏手机号，再依次调用 `send_sms`、`verify_sms` 与 `login` |
| `reauthenticate` | 无 | `null` | 清除本地缓存的令牌与 MFA 状态，强制重新执行完整密码认证 |
| `logout` | 无 | `null` | 清空本地令牌、MFA 状态与登录标记；共享该会话的教务、校园卡与校园邮箱客户端在下次读取 CAS 令牌时将收到 `not_authenticated` |
| `is_logged_in` | 无 | `boolean` | 返回当前 CAS 会话是否处于已登录状态 |
| `device_id` | 无 | `string` | 返回创建客户端时配置的设备 ID |
| `set_tokens` | `userToken: string`（兼容别名 `idToken`）、`refreshToken: string` | `null` | 预置外部持久化的 JWT 令牌对并重置登录态，调用后**仍须调用 `login`** 激活会话。若令牌格式非法或已过期，返回 `ZZU_INVALID_INPUT` 而非 `token_expired` |
| `tokens` | 无 | `{"userToken": string, "refreshToken": string} \| null` | 读取当前缓存的令牌对；未设置且未登录时返回 `null` |
| `user_token` | 无 | `string` | 本地校验并返回可用的 `userToken`；未登录返回 `not_authenticated`，剩余有效期不足 15 分钟或 `refreshToken` 已过期返回 `token_expired` |
| `detect_mfa` | 无 | `{"isRequired": boolean, "canUseSms": boolean, "isVerified": boolean}` | 向服务端探测当前账号与设备是否需要 MFA 以及是否支持短信验证；每次调用都会重置未完成的 MFA 状态 |
| `prepare_sms` | 无 | `{"securePhone": string \| null}` | 初始化短信 MFA 会话并返回脱敏手机号，不发送短信。未探测 MFA 时会先自动探测；同一轮验证内重复调用直接返回缓存结果。无需 MFA 或不支持短信时返回 `invalid_mfa_state`；收到服务端拒绝或无效响应时自动清空 MFA 状态 |
| `send_sms` | 无 | `{"securePhone": string \| null}` | 向绑定安全手机发送短信验证码并返回脱敏手机号。未探测或未初始化时会自动完成 `prepare_sms`；无需 MFA 或不支持短信时返回 `invalid_mfa_state`。发送失败时会保留当前 MFA 会话，避免作废已发验证码；若状态已过期，可调用 `detect_mfa` 重置 |
| `verify_sms` | `code: string` | `null` | 提交非空短信验证码完成校验，通过后须再次调用 `login`。未先调用 `send_sms` 时返回 `invalid_mfa_state`；验证码错误返回 `mfa_verification_failed`，并保留当前会话供直接重试；服务端返回非 2xx 或业务失败时自动清空 MFA 状态 |
| `set_device_trust` | `deviceId: string`、`isTrusted: boolean` | `null` | 更新当前账号下指定设备的信任状态（受信任设备登录时无须 MFA）（需已登录，`deviceId` 不得为空）。`true` 表示信任，`false` 表示取消信任 |
| `user_info` | 无 | `PersonalInfo` | 查询个人身份信息（需已登录） |
| `service_sso_url` | `service: Service` | `string` | 获取指定业务系统的单点登录跳转 URL（需已登录；返回的 URL 携带登录票据或用户令牌，不得写入日志） |

`Service` 参数取值：

- 独立系统（字符串）：`"email"`（校园邮箱）、`"library"`（图书馆门户）、`"library_reservation"`（图书馆空间预约）、`"leave"`（学生请假系统）、`"president_mailbox"`（校长信箱）、`"ecard"`（校园卡）。其中 `"president_mailbox"` 与 `"ecard"` 在本地直接用当前 `userToken` 拼接 `token=...` 参数，不发起网络请求；其余四项向门户或 CAS 请求单点登录重定向并原样提取响应 `Location` 头，缺少非空 `Location` 头时返回 `ZZU_DECODE_ERROR`。
- 本科教务子应用（对象 `{"undergrad_eas": "<app>"}`）：在本地用当前 `userToken` 直接拼接目标教务子应用的 `?idToken=...` 地址，不发起网络请求。`"<app>"` 可取 `"student_status"`（学籍信息查询）、`"program_completion"`（培养方案完成情况）、`"exams"`（考试查询）、`"academic_warnings"`（学业预警查询）、`"grades"`（成绩查询）、`"timetable"`（课表查询）、`"course_offerings"`（全校开课查询）、`"school_calendar"`（校历）、`"available_classrooms"`（空闲教室查询）、`"classroom_timetable"`（教室课表）。

`PersonalInfo` 结构：

```json
{
  "uid": "学号或工号 (string)",
  "name": "姓名 (string)",
  "identityTypeName": "身份类别名称 (string)",
  "identityTypeId": "身份类别编码 (string)",
  "organizationName": "所属院系或部门名称 (string)",
  "organizationId": "所属院系或部门编码 (string)"
}
```

### `ZzuEasClient`（本科教务）

除 `login`、`logout`、`is_logged_in` 外，其余方法均要求先完成本客户端的 `login`（否则返回 `ZZU_AUTHENTICATION_ERROR`，`kind: "not_authenticated"`）。

| 方法名 | 参数 JSON 字段 | 结果 JSON 类型 | 行为说明 |
| --- | --- | --- | --- |
| `login` | 无 | `null` | 校验教务门户会话并拉取、缓存当前学期元数据（需先完成 `ZzuCasClient` 的 `login`） |
| `logout` | 无 | `null` | 清除教务客户端缓存的当前学期登录态（不影响底层 `ZzuCasClient`） |
| `is_logged_in` | 无 | `boolean` | 返回教务客户端是否已完成登录 |
| `current_semester` | 无 | `Semester` | 直接返回 `login` 时缓存的当前学期对象（无网络请求） |
| `semesters` | 无 | `Semester[]` | 查询当前学生可见的全部学期列表 |
| `timetable` | `semesterId?: int64`、`includeExperiments?: boolean` | `Timetable` | 查询指定学期（省略时取当前学期）的完整课表，内含匹配的每日节次作息、全部教学班与排课场次；`includeExperiments` 默认 `true`；`semesterId` 不存在时返回 `ZZU_NOT_FOUND` |
| `classroom_catalog` | 无 | `ClassroomCatalog` | 并发查询教室占用筛选所需的全部参照目录（校区、教学楼、教室类型、楼层与标准节次作息），返回扁平列表供前端渲染筛选项、绘制节次概览及按 `buildingId` 反查教学楼信息 |
| `classroom_occupancies_page` | `date: "YYYY-MM-DD"`、`campusId?: int64`、`buildingIds?: int64[]`、`roomTypeIds?: int64[]`、`floors?: int32[]`、`minSeats?: uint32`、`maxSeats?: uint32`、`page?: uint32`、`pageSize?: uint32` | `ClassroomPage` | 按页查询符合筛选条件的教室及当日占用记录。`page` 从 `1` 开始（默认 `1`），`pageSize` 默认 `44`；`page == 0`、`pageSize == 0` 或 `minSeats > maxSeats` 返回 `ZZU_INVALID_INPUT`。**性能警告**：上游 API 存在极其严重的性能问题，响应耗时随 `pageSize` 增大而非线性增大（`100` 约 1 秒以上，`1000` 约 25 秒以上），`44` 左右通常是单条均摊耗时最低的甜点值；不清楚后端特性时请保持默认，高占用率校区可酌情调小以降低单页尾延迟 |
| `classroom_occupancies` | `date: "YYYY-MM-DD"`、`campusId?: int64`、`buildingIds?: int64[]`、`roomTypeIds?: int64[]`、`floors?: int32[]`、`minSeats?: uint32`、`maxSeats?: uint32`、`pageSize?: uint32`、`concurrency?: uint64` | `ClassroomOccupancy[]` | 先以 `pageSize`（默认 `44`）拉取第 1 页获知总页数，再以 `concurrency`（默认 `8`）为并发上限并发拉取剩余分页，按页序合并并按教室 `id` 去重（任一页失败立即中止并返回错误）。`pageSize == 0`、`concurrency == 0` 或 `minSeats > maxSeats` 返回 `ZZU_INVALID_INPUT`。不带空间筛选的整校查询（约 1658 间教室）在高占用时段可能接近默认 `15000` 毫秒时限，建议配合校区/教学楼筛选、放宽 `timeoutMs`，或在需要进度条与单页容错时改用 `classroom_occupancies_page` |

离线教学周换算规则（SDK 可直接基于 `Semester` 在本地计算，无需发起网络请求）：
- 若目标公历日期 `date` 小于 `startDate` 或大于 `endDate`，该日期不属于本学期。
- 按 `weekStartOnSunday` 分别将 `startDate` 与 `date` 回退到所在周的周首日（`true` 回退至周日，`false` 回退至周一）。
- 取 `offset = (dateWeekStart - startWeekStart) / 7`，对应 `weekIndices[offset]` 即为该日期所属教学周序号；同理，第 `k` 项周次的完整 7 天区间为 `[startWeekStart + 7 * k, startWeekStart + 7 * k + 6]`。

教务数据模型：

| 类型 | JSON 字段与类型 | 说明 |
| --- | --- | --- |
| `Semester` | `id: int64`、`code: string`、`nameZh: string`、`nameEn: string \| null`、`schoolYear: string`、`season: string`、`startDate: "YYYY-MM-DD"`、`endDate: "YYYY-MM-DD"`、`weekStartOnSunday: boolean`、`countInTerm: boolean`、`weekIndices: uint32[]` | 学期校历元数据；`season` 已知取值为 `"spring"`、`"autumn"`、`"summer"`、`"winter"`（须容忍未知字符串）；`weekIndices` 非零且唯一 |
| `Timetable` | `semester: Semester`、`units: CourseUnit[]`、`classes: TeachingClass[]`、`sessions: ClassSession[]` | 单个学期的完整课表；`units` 为本课表适用的每日节次作息（按 `index` 升序排列，作为 `zzu_timetable_to_calendar` 入参时可省略）；`sessions` 按上课时间升序排列，每条 `session.classId` 均存在于 `classes` 中 |
| `CourseUnit` | `index: uint8`、`nameZh: string`、`nameEn: string \| null`、`dayPart: string`、`segmentIndex: uint8`、`startTime: "HH:MM:SS"`、`endTime: "HH:MM:SS"` | 单节作息时间与时段定义；`index` 与 `segmentIndex` 均从 `1` 起；`dayPart` 已知取值为 `"morning"`、`"noon"`、`"afternoon"`、`"evening"`（须容忍未知字符串）；`endTime` 严格晚于 `startTime` |
| `TeachingClass` | `id: int64`、`code: string`、`origin: "regular" \| "standalone_experiment"`、`course: Course`、`courseType: NamedEntity \| null`、`cultivateType: NamedEntity \| null`、`department: NamedEntity \| null`、`campus: NamedEntity \| null`、`teachers: TeacherName[]`、`studentCount: uint32 \| null`、`scheduleState: string`、`scheduleText: string \| null`、`scheduleTextEn: string \| null`、`remark: string \| null` | 学期教学班元数据（含未排课课程与实验父班）；`scheduleState` 已知取值为 `"published"`、`"unpublished"`（须容忍未知字符串） |
| `Course` | `id: int64`、`code: string`、`nameZh: string`、`nameEn: string \| null`、`credits: float64 \| null`、`periodInfo: PeriodInfo` | 课程基础元数据；独立实验课程的 `credits` 可能为 `null` |
| `PeriodInfo` | `total: uint32`、`weeks: uint32 \| null`、`periodsPerWeek: uint32 \| null`、`theory: uint32 \| null`、`experiment: uint32 \| null`、`practice: uint32 \| null`、`design: uint32 \| null`、`machine: uint32 \| null`、`test: uint32 \| null` | 总学时、开课周数、周学时与分项学时统计 |
| `TeacherName` | `nameZh: string`、`nameEn: string \| null` | 教学班任课教师姓名 |
| `ClassSession` | `classId: int64`、`scheduleGroupId: int64`、`weekIndex: uint32`、`date: "YYYY-MM-DD"`、`originalDate: "YYYY-MM-DD" \| null`、`weekday: uint8`（`1..=7`）、`startTime: string`、`endTime: string`、`startUnit: uint8`、`endUnit: uint8`、`startUnitNameZh: string \| null`、`endUnitNameZh: string \| null`、`teachers: SessionTeacher[]`、`room: Room \| null`、`customPlace: string \| null`、`kind: string`、`state: string \| null`、`periods: uint32 \| null`、`experimentProject: ExperimentProject \| null` | 单次上课场次；同教学班同时间同地点的多人合上记录已合并进 `teachers`；`startTime`、`endTime` 为 `Asia/Shanghai` 时区 RFC 9557 有效起止时间；`startUnit` 与 `endUnit` 从 `1` 起且不限 `10`；`kind` 已知取值为 `"theory"`、`"experiment"`、`"practice"`、`"machine"`、`"design"`；`state` 已知取值为 `"pending"`、`"completed"`（均须容忍未知字符串） |
| `SessionTeacher` | `id: int64 \| null`、`personId: int64 \| null`、`nameZh: string`、`nameEn: string \| null` | 单场排课的授课教师 |
| `ExperimentProject` | `id: int64`、`code: string`、`nameZh: string`、`totalPeriods: uint32 \| null` | 实验排课关联的具体实验项目详情 |
| `Room` | `id: int64`、`nameZh: string`、`nameEn: string \| null`、`building: NamedEntity`、`campus: NamedEntity`、`seatNumber: string \| null` | 上课教室及其所属教学楼与校区 |
| `NamedEntity` | `id: int64`、`nameZh: string`、`nameEn: string \| null`、`code: string` | 通用教务命名实体 |
| `CalendarExportOptions` | `weeks?: uint32[] \| null`、`classIds?: int64[] \| null`、`startDate?: "YYYY-MM-DD" \| null`、`endDate?: "YYYY-MM-DD" \| null`、`alarmMinutesBefore?: uint32[]`、`calendarName?: string \| null` | `zzu_timetable_to_calendar` 可选导出配置；`weeks` 不得含 `0`，`startDate` 不得晚于 `endDate` |
| `ClassroomCatalog` | `campuses: Campus[]`、`buildings: Building[]`、`roomTypes: RoomType[]`、`floors: int32[]`、`units: CourseUnit[]` | 教室占用筛选参照目录与标准节次作息快照；`units` 按 `index` 升序排列 |
| `Campus` | `id: int64`、`code: string`、`nameZh: string`、`nameEn: string \| null`、`remark: string \| null`、`enabled: boolean` | 校区元数据 |
| `Building` | `id: int64`、`code: string`、`nameZh: string`、`nameEn: string \| null`、`campusId: int64`、`remark: string \| null`、`enabled: boolean` | 教学楼元数据；`campusId` 映射自上游 `campusAssoc`（可能指向未出现在 `campuses` 中的默认校区 ID） |
| `RoomType` | `id: int64`、`code: string`、`nameZh: string`、`nameEn: string \| null`、`enabled: boolean` | 教室类型元数据 |
| `ClassroomPage` | `items: ClassroomOccupancy[]`、`page: uint32`、`pageSize: uint32`、`totalItems: uint32`、`totalPages: uint32` | 单页教室占用查询结果；`pageSize` 为本次实际使用的页大小，SDK 不应假定其恒为 `44` |
| `ClassroomOccupancy` | `id: int64`、`code: string`、`nameZh: string`、`nameEn: string \| null`、`buildingId: int64`、`campusNameZh: string`、`occupations: RoomOccupation[]` | 教室信息与当日占用列表（无占用时为空数组） |
| `RoomOccupation` | `activityId: int64 \| null`、`activityType: string`、`activityName: string`、`activityNameEn: string \| null`、`date: "YYYY-MM-DD"`、`startTime: string`、`endTime: string`、`teacherName: string`、`teacherNameEn: string \| null` | 单条教室占用记录；`startTime`、`endTime` 为 `Asia/Shanghai` 时区的 RFC 9557 字符串（左闭右开区间 `[startTime, endTime)`）；`activityType` 已知取值为 `"lesson"`、`"experiment"`、`"room_borrow"`、`"exam"`、`"schedule_change"`，封装层须容忍上游新增的其他字符串取值 |

### `ZzuEcardClient`（校园卡与电控）

除 `login`、`logout`、`is_logged_in`、`payment_channels`、`prepare_payment` 外，其余方法均要求先完成本客户端的 `login`。

| 方法名 | 参数 JSON 字段 | 结果 JSON 类型 | 行为说明 |
| --- | --- | --- | --- |
| `login` | 无 | `null` | 使用底层 `ZzuCasClient` 令牌换取校园卡服务会话（需先完成 CAS `login`） |
| `logout` | 无 | `null` | 清除本地校园卡会话与缓存的默认房间（不影响底层 `ZzuCasClient`） |
| `is_logged_in` | 无 | `boolean` | 返回校园卡客户端是否已建立会话 |
| `default_room` | 无 | `string` | 查询并缓存当前账号绑定的默认宿舍房间号（格式为 `area-building--level-room`） |
| `balance` | 无 | `string` | 查询校园卡账户余额（单位：元，十进制字符串，如 `"12.34"`） |
| `payment_code` | 无 | `PaymentCode` | 获取在线校园码（动态付款码与通行码）及服务端建议刷新秒数；在线码未开放时返回 `ZZU_REMOTE_ERROR` |
| `transactions` | `startDate: "YYYY-MM-DD"`、`endDate: "YYYY-MM-DD"`、`kind?: "consumption" \| "recharge" \| "subsidy"`、`page?: uint32`、`pageSize?: uint32` | `Transaction[]` | 分页查询指定日期闭区间内的校园卡账单流水。`kind` 省略时查询全部类别；`page` 从 `1` 开始（默认 `1`），`pageSize` 默认 `30`；`page == 0`、`pageSize == 0` 或 `startDate > endDate` 返回 `ZZU_INVALID_INPUT`。远端不返回总条数，翻页至返回数组长度小于 `pageSize`（或为空数组）即到达末页。`recharge_energy` 超时或取消后可通过本方法核对最新扣款记录 |
| `locations` | `parent: string` | `{"id": string, "name": string}[]` | 按层级检索电控位置目录（按 `id` 字典序升序排列，`area`、`building`、`level` 各段均非空且不含 `-`）：`parent` 依次传 `""`（区域）、`"area"`（如 `"1"`，楼栋）、`"area-building"`（如 `"1-2"`，单元/楼层）、`"area-building--level"`（如 `"1-2--3"`，房间，返回的 `id` 为合法房间号）；格式不符返回 `ZZU_INVALID_INPUT` |
| `remaining_energy` | `room?: string` | `number` | 查询指定房间（省略时取 `default_room`）的剩余电量（`float64`，单位：度） |
| `recharge_energy` | `paymentPassword: string`、`amount: integer`、`room: string` | `null` | 使用校园卡支付密码从卡余额扣款充入电费；`amount` 为正整数元（`uint32`），`room` 为合法房间号。若响应缺失确认字段将返回 `ZZU_DECODE_ERROR`，加密响应解密失败将返回 `ZZU_CRYPTO_ERROR`，两者扣款结果均未知，重试前须先查余额或核对账单流水 |
| `recharge_config` | 无 | `{"amounts": uint32[], "isBrowserPaymentEnabled": boolean}` | 查询校园卡充值可选面额列表（元）及网页收银台充值开关状态 |
| `create_recharge_order` | `amount: integer` | `Order` | 创建指定正整数元（`uint32`）的校园卡充值订单；网页充值未开放时返回 `ZZU_REMOTE_ERROR` |
| `payment_channels` | `order: Order` | `Channel[]` | 查询该订单在学校收银台可用的支付渠道列表 |
| `prepare_payment` | `order: Order`、`channel: Channel`、`context?: PaymentContext` | `PaymentAction` | 向收银台预下单并提取最终拉起第三方支付的跳转或表单动作 |

校园码、账单流水、充值与支付数据结构：

- **`PaymentCode`**：`{"code": string, "balance": string, "refreshSeconds": uint32}`。其中 `code` 为用于渲染二维码图像的原始动态码字符串（含实时支付凭据，**严禁写入日志**），`balance` 为当前校园卡余额（十进制元字符串），`refreshSeconds` 为建议自动刷新周期（秒，如 `60`）。
- **`Transaction`**：`{"id": string, "time": string, "amount": string, "merchantName": string, "summary": string, "isRefund": boolean}`。其中 `id` 为交易流水号，`time` 为 `Asia/Shanghai` 时区的 RFC 9557 时间字符串，`amount` 为带符号精确十进制元字符串（消费通常为负数如 `"-12.50"`，充值与补助通常为正数如 `"50.00"`），`isRefund` 表示是否为退款记录。

- **`Order`**：包含公开字段 `id`（`string`，订单号）、`amount`（`uint32`，充值金额，单位元）、`checkoutUrl`（`string`，默认收银台 HTTPS 地址）以及内部不透明字段 `payment`。调用 `payment_channels`、`prepare_payment` 或 `zzu_recharge_order_checkout_url_for` 时将整个 `Order` JSON 对象原样传回即可；该对象可序列化保存并在新进程中恢复以继续支付，但内部含支付凭据，**严禁写入日志**。
- **`Channel`**：`{"id": string, "name": string, "accountId": string | null}`。作为入参传回时 `id`（非空）与 `name` 必填，`accountId` 可省略或传 `null`。
- **`PaymentContext`**：`{"device?": "android" | "ios", "payerIp?": string | null}`，整个对象及各字段均可省略。`device` 默认 `"android"`；`payerIp` 为 IPv4/IPv6 地址字符串，省略时按空字符串上报。
- **`PaymentAction`**：按 `kind` 标签区分两种联合体结构：
  - URL 跳转：`{"kind": "redirect", "url": "https://...", "referrer": "https://..." | null}`（如微信 H5 支付，发起跳转请求时若 `referrer` 非空须携带该 `Referer` 请求头）。
  - 表单提交：`{"kind": "submit_form", "url": "https://...", "fields": [{"name": "...", "value": "..."}]}`（如支付宝网关，须按 `POST` 提交表单；`fields` 中可能存在同名字段，封装层**必须保留重复项与原始顺序**，不得用字典/哈希表去重）。

### `ZzuMailClient`（校园邮箱）

除 `login`、`logout`、`is_logged_in` 外，其余方法均要求先完成本客户端的 `login`（未登录时返回 `ZZU_AUTHENTICATION_ERROR`，`kind: "not_authenticated"`）。

| 方法名 | 参数 JSON 字段 | 结果 JSON 类型 | 行为说明 |
| --- | --- | --- | --- |
| `login` | 无 | `null` | 使用底层 `ZzuCasClient` 令牌换取邮箱会话（`sid`）与网页版入口地址（需先完成 CAS `login`） |
| `logout` | 无 | `null` | 清除本地缓存的邮箱会话（不影响底层 `ZzuCasClient`） |
| `is_logged_in` | 无 | `boolean` | 返回校园邮箱客户端是否已建立本地会话 |
| `webmail_url` | 无 | `string` | 返回当前邮箱会话对应的网页版入口 URL（含 `sid` 会话凭据，**不得写入日志**）；会话超期或 CAS 令牌变更时自动换新 |
| `unread_count` | 无 | `uint32` | 查询收件箱（文件夹 ID 为 `1`）中的未读邮件数（不计入虚拟视图、草稿箱、已发送、已删除、垃圾邮件与病毒文件夹） |

### `ZzuPortalClient`（校园网 Portal 认证）

构建 `ZzuPortalClient` 前，通常先调用独立导出函数 `zzu_portal_discover`（或 `zzu_portal_discover_blocking`）探测当前强制门户参数：

- **探测结果**：`{"authUrl": string, "portalServerUrl": string, "userIp": string}`。若当前网络已连通互联网、未触发门户重定向，探测返回 `ZZU_REMOTE_ERROR`。

| 方法名 | 参数 JSON 字段 | 结果 JSON 类型 | 行为说明 |
| --- | --- | --- | --- |
| `login` | `account: string`、`password: string`、`encryption?: "plain" \| "xor"` | `PortalAuth` | 向校园网 `eportal` 发起终端上线认证；`encryption` 默认 `"plain"`，选 `"xor"` 时待编码字段不得超过 512 个字符。融合宽带账号须在 `account` 后自行附带运营商后缀（如 `@cmcc`、`@unicom`、`@telecom`，可通过 `isps` 查询） |
| `isps` | 无 | `IspInfo[]` | 查询当前校园网 Portal 页面提供的运营商列表及其登录账号后缀；若门户页面模板不含运营商选项则返回空数组 `[]` |

`PortalAuth` 结构为 `{"isSuccess": boolean, "result": int64, "message": string, "retCode": int64 | null}`。注意：只要 HTTP 请求与 JSONP 报文正常解析，本调用即返回 `ZZU_OK`；终端认证是否通过须检查结果中的 **`isSuccess`**（即 `result == 1`）。

`IspInfo` 结构为 `{"name": string, "suffix": string}`，其中 `name` 为运营商显示名称，`suffix` 为附在登录账号后的运营商后缀（普通校园网通常为空字符串 `""`）。

### `ZzuSelfServiceClient`（校园网自助服务）

除 `login`、`is_logged_in` 外，其余方法均要求先完成 `login`（未登录时返回 `ZZU_AUTHENTICATION_ERROR`，`kind: "not_authenticated"`）。

| 方法名 | 参数 JSON 字段 | 结果 JSON 类型 | 行为说明 |
| --- | --- | --- | --- |
| `login` | `account: string`、`password: string` | `null` | 登录校园网自助服务系统（`/Self`） |
| `is_logged_in` | 无 | `boolean` | 返回当前自助服务客户端是否已登录 |
| `online_devices` | 无 | `OnlineDevice[]` | 查询当前账号下的全部在线终端列表 |
| `kick_device` | `sessionId: string` | `null` | 按 `OnlineDevice.sessionId`（非空）强制指定终端下线 |
| `logout` | 无 | `null` | 登出远端自助服务会话并清除本地登录标记 |

`OnlineDevice` 结构：

```json
{
  "brasid": "1",
  "downFlow": "1024",
  "hostName": "device-host",
  "ip": "10.1.2.3",
  "loginTime": "2026-10-31T08:30:45+08:00[Asia/Shanghai]",
  "mac": "001122334455",
  "sessionId": "sid-1",
  "terminalType": "PC",
  "upFlow": "256",
  "useTime": 3661,
  "userId": 42
}
```

其中 `loginTime` 为 `Asia/Shanghai` 时区的 RFC 9557 时间字符串，`useTime` 为已在线秒数（`uint64`），`userId` 为整数账户 ID（`int64`），`hostName` 未上报时为空字符串 `""`。

---

## 本地数据函数

以下三个导出函数对应 `zzu` 数据模型上的纯计算逻辑，无需 `ZzuRuntime`、客户端句柄或登录态，不发起网络请求，在当前线程同步返回。返回 `ZZU_OK` 时 `*out_payload` 为结果 JSON，否则为错误 JSON（`out_payload` 传 `NULL` 时返回 `ZZU_INVALID_INPUT`）。

| 函数 | 输入字节切片 | 成功结果 JSON | 说明 |
| --- | --- | --- | --- |
| `zzu_timetable_to_calendar` | UTF-8 JSON 对象 `{"timetable": Timetable, "options"?: CalendarExportOptions}` | `string`（`.ics` 日历文本） | `timetable` 原样传入 `timetable` 方法的返回值，`options` 可省略。结果载荷是 JSON 字符串，落盘为 `.ics` 文件前须先完成 JSON 字符串解码。`DTSTAMP` 取渲染时刻，同一课表多次渲染的输出不会逐字节相同 |
| `zzu_room_id_parse` | 原始 UTF-8 房间号文本（**非** JSON 字符串，如直接传 `1-2--3-301` 的 10 个字节） | `string`（如 `"1-2--3-301"`） | 校验房间号是否符合 `area-building--level-room` 格式（四段均非空且不含 `-`）；格式非法返回 `ZZU_INVALID_INPUT` |
| `zzu_recharge_order_checkout_url_for` | UTF-8 JSON 对象 `{"order": Order, "channel": Channel}` | `string`（HTTPS URL） | 基于 `create_recharge_order` 返回的 `Order` 生成预选该支付渠道的学校网页收银台地址 |

---

## SDK 封装与平台接入

### 跨语言封装范式

各宿主语言封装 `libzzu` 时建议统一采用以下模式：

1. **进程级静态回调加整数令牌表**：全进程只向原生层注册一个静态 C 回调函数，每次异步调用分配一个递增整数 token 作为 `user_data`（转为 `uintptr_t` / `void *`），在语言侧维护线程安全的 `token -> Promise/Future/Completer` 映射表。这样既避免闭包委托被垃圾回收器提前回收，也无需将托管对象引用固定（pin）在原生内存中。
2. **极简回调体**：回调内部仅完成三件事——读取并复制 `ZzuBuffer` 字节（或转移指针）、调用 `zzu_buffer_free`、唤醒或投递结果到宿主语言自身的事件循环/调度器；JSON 反序列化与业务异常转换全部放在宿主线程中执行。
3. **异常兜底与未知码容错**：在静态回调最外层捕获所有托管异常或运行时错误，严禁异常穿透 C 边界；状态码映射时将未识别的 `ZzuStatus` 统一归为 `ZZU_UNKNOWN_ERROR`。
4. **生命周期映射**：将 `ZzuRuntime` 封装为进程级惰性单例；将六个客户端封装为各自独立的资源管理类型（如 Java/Kotlin `AutoCloseable`、C# `IDisposable`、Python 上下文管理器、Swift `deinit`），不要合并为大一统的“上帝客户端”。
5. **RFC 9557 时间兼容**：Java `ZonedDateTime.parse` 可直接解析带 `[Asia/Shanghai]` 后缀的字符串；Python、C#、Go、Dart 等标准库仅识别 RFC 3339，解析前剥离末尾 `[...]` 方括号段即可。注意各语言时间精度与时区差异（Python 与 Dart 截断至微秒，C# 舍入至 100 纳秒，Java 与 Go 保留纳秒；Dart `DateTime.parse` 会转换为 UTC，需展示北京时间时应保留原始偏移）。

### Android 平台初始化

Android 上 `zzu_runtime_new` **必须**提供 `rootCertificatesPem`，否则返回 `ZZU_INVALID_INPUT`。不走平台校验器，是因为 `rustls-platform-verifier` 的 Android 实现会在工作线程上同步下载并解析吊销列表，遇到大体积 CRL 时会占住工作线程，连超时也无法触发。

宿主从系统公开接口 `AndroidCAStore` 取别名以 `system:` 开头的证书，拼成 PEM 后传入：

```kotlin
val store = KeyStore.getInstance("AndroidCAStore").apply { load(null) }
val encoder = Base64.getMimeEncoder(64, "\n".toByteArray())
val pem = store.aliases().asSequence()
    .filter { it.startsWith("system:") }
    .joinToString("\n") { alias ->
        val body = encoder.encodeToString(store.getCertificate(alias).encoded)
        "-----BEGIN CERTIFICATE-----\n$body\n-----END CERTIFICATE-----"
    }
```

提供根证书后由 rustls 直接校验，因此不做吊销检查，也不遵循应用的 `network_security_config`。
