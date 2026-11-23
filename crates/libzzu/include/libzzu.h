#ifndef LIBZZU_H
#define LIBZZU_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * C ABI 主版本号：出现破坏性变更时递增。
 */
#define ZZU_ABI_VERSION_MAJOR 1

/**
 * C ABI 次版本号：新增方法或字段时递增，主版本号递增时归零。
 */
#define ZZU_ABI_VERSION_MINOR 0

/**
 * 本库分配的字节缓冲区不透明句柄，内容为 UTF-8 JSON（不含结尾 `NUL`），须通过 `zzu_buffer_free` 释放。
 */
typedef struct ZzuBuffer ZzuBuffer;

/**
 * 一次在途调用的不透明句柄，仅用于取消，须通过 `zzu_call_free` 释放。
 */
typedef struct ZzuCall ZzuCall;

/**
 * 统一身份认证（CAS）客户端不透明句柄，线程安全，须通过 `zzu_cas_free` 释放。
 */
typedef struct ZzuCasClient ZzuCasClient;

/**
 * 本科教务客户端不透明句柄，线程安全，须通过 `zzu_eas_free` 释放。
 */
typedef struct ZzuEasClient ZzuEasClient;

/**
 * 校园卡与电控客户端不透明句柄，线程安全，须通过 `zzu_ecard_free` 释放。
 */
typedef struct ZzuEcardClient ZzuEcardClient;

/**
 * 校园网 Portal 认证客户端不透明句柄，线程安全，须通过 `zzu_portal_free` 释放。
 */
typedef struct ZzuPortalClient ZzuPortalClient;

/**
 * 后台异步运行时不透明句柄，可供多个客户端共享，须通过 `zzu_runtime_free` 释放。
 */
typedef struct ZzuRuntime ZzuRuntime;

/**
 * 校园网自助服务客户端不透明句柄，线程安全，须通过 `zzu_self_service_free` 释放。
 */
typedef struct ZzuSelfServiceClient ZzuSelfServiceClient;

/**
 * C ABI 统一使用的状态码类型（`ZZU_OK` 表示成功，非零表示失败大类）。
 *
 * 封装层应把未识别的取值当作 `ZZU_UNKNOWN_ERROR` 处理。
 */
typedef int32_t ZzuStatus;

/**
 * 调用完成回调，必须使用 C 调用约定且不得让异常或 panic 穿出。
 *
 * 回调运行在后台工作线程上，须尽快返回；`status` 为 `ZZU_OK` 时 `payload` 为结果 JSON，
 * 否则为错误 JSON。`payload` 恒非空，其所有权移交给回调方，须通过 `zzu_buffer_free` 释放，
 * 可传往其他线程并在回调返回后继续使用。
 */
typedef void (*ZzuCallback)(void *user_data, ZzuStatus status, struct ZzuBuffer *payload);

/**
 * 操作成功完成。
 */
#define ZZU_OK 0

/**
 * 调用方传入了空指针、非 UTF-8 文本、非法 JSON 或取值非法的参数。
 */
#define ZZU_INVALID_INPUT 1

/**
 * 网络连接或底层传输失败。
 */
#define ZZU_TRANSPORT_ERROR 2

/**
 * 服务端返回非成功的 HTTP 状态码。
 */
#define ZZU_HTTP_ERROR 3

/**
 * 响应报文或协议数据解码失败。
 */
#define ZZU_DECODE_ERROR 4

/**
 * 身份认证或多因素认证失败（细分原因见错误 JSON 的 `kind` 字段）。
 */
#define ZZU_AUTHENTICATION_ERROR 5

/**
 * 远端服务返回业务级拒绝或失败结果。
 */
#define ZZU_REMOTE_ERROR 6

/**
 * 请求的目标资源（如指定学期 ID）不存在。
 */
#define ZZU_NOT_FOUND 7

/**
 * 密钥解析或加解密处理失败。
 */
#define ZZU_CRYPTO_ERROR 8

/**
 * 内部异步运行时初始化失败。
 */
#define ZZU_RUNTIME_ERROR 9

/**
 * Rust 内部发生 panic，或客户端先前已因 panic 损坏；须释放该客户端句柄并重建。
 */
#define ZZU_PANIC 10

/**
 * 未归类的未知错误。
 */
#define ZZU_UNKNOWN_ERROR 11

/**
 * 单次操作超过配置的总时限。
 */
#define ZZU_TIMEOUT 12

/**
 * 操作已由 `zzu_call_cancel` 取消。
 */
#define ZZU_CANCELLED 13

/**
 * 当前链接库不支持该方法名（通常意味着封装层比链接库新）。
 */
#define ZZU_UNKNOWN_METHOD 14

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * 返回链接库实现的 C ABI 版本，编码为 `(major << 16) | minor`。
 *
 * 封装层应在加载时校验：主版本号与头文件相等，且次版本号不小于所需版本。
 */
uint32_t zzu_abi_version(void);

/**
 * 返回链接库的软件版本号，为静态存储的 `NUL` 结尾字符串，不得释放。
 */
const char *zzu_version(void);

/**
 * 返回缓冲区首字节地址（`buffer` 为 `NULL` 时返回 `NULL`），不保证以 `NUL` 结尾。
 *
 * 返回的指针在调用 `zzu_buffer_free` 前有效；长度须通过 `zzu_buffer_len` 获取。
 *
 * # Safety
 *
 * `buffer` 必须为 `NULL`，或指向尚未释放的有效 `ZzuBuffer` 句柄。
 */
const uint8_t *zzu_buffer_data(const struct ZzuBuffer *buffer);

/**
 * 返回缓冲区字节长度（`buffer` 为 `NULL` 时返回 `0`）。
 *
 * # Safety
 *
 * `buffer` 必须为 `NULL`，或指向尚未释放的有效 `ZzuBuffer` 句柄。
 */
size_t zzu_buffer_len(const struct ZzuBuffer *buffer);

/**
 * 释放 `ZzuBuffer`（传入 `NULL` 时无操作），可在任意线程调用。
 *
 * # Safety
 *
 * `buffer` 必须为 `NULL`，或由本库返回且尚未释放的句柄；释放后不得再访问其数据指针。
 */
void zzu_buffer_free(struct ZzuBuffer *buffer);

/**
 * 请求取消一次在途异步调用，可在任意线程重复调用；操作已结束时为空操作。
 *
 * 取消尽力而为：回调仍保证恰好触发一次，状态码通常为 `ZZU_CANCELLED`，
 * 若完成与取消发生竞态也可能收到真实执行结果。非幂等操作（如电费充值）取消后服务端可能已受理，重试前须核实余额。
 *
 * # Safety
 *
 * `call` 必须为 `NULL`，或指向尚未释放的有效 `ZzuCall` 句柄。
 */
void zzu_call_cancel(const struct ZzuCall *call);

/**
 * 释放调用句柄（传入 `NULL` 时无操作）；不会取消调用，回调仍照常触发。
 *
 * # Safety
 *
 * `call` 必须为 `NULL`，或由 `zzu_*_call` 写入且尚未释放的句柄；释放后不得再使用。
 */
void zzu_call_free(struct ZzuCall *call);

/**
 * 创建统一身份认证客户端（本地完成，不发起网络请求）。
 *
 * `config` 为 UTF-8 JSON 对象：`account`、`password` 必填，`timeoutMs` 可选（单次操作总时限，单位毫秒，默认 `15000`）。
 *
 * # Safety
 *
 * - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
 * - `config` 在 `config_len` 非零时必须指向调用期间可读的 `config_len` 字节。
 * - `out_cas` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_cas_free` 释放。
 * - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_cas_new(const struct ZzuRuntime *runtime,
                      const uint8_t *config,
                      size_t config_len,
                      struct ZzuCasClient **out_cas,
                      struct ZzuBuffer **out_error);

/**
 * 释放 CAS 客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
 *
 * 已由它创建的 `ZzuEasClient` 与 `ZzuEcardClient` 不受影响，在途调用的回调仍会照常触发。
 *
 * # Safety
 *
 * `cas` 必须为 `NULL`，或由 `zzu_cas_new` 返回且尚未释放的句柄；释放后不得再使用。
 */
void zzu_cas_free(struct ZzuCasClient *cas);

/**
 * 异步调用 CAS 客户端方法；立即返回，绝不在当前调用栈内同步触发回调。
 *
 * 返回 `ZZU_OK` 时 `callback` 必定在后台工作线程上恰好触发一次；触发可能早于本函数返回，但保证在写入 `*out_call` 之后。
 * 返回非零状态码时回调绝不触发。同步返回值仅校验句柄、方法名、回调与输入切片指针；`params` JSON 解析及后续执行错误一律经回调上报。
 *
 * `method` 为 `NUL` 结尾的方法名（如 `"login"`）；`params` 为 UTF-8 JSON 对象，`params_len` 为 `0` 表示无参数（此时 `params` 可传 `NULL`）。
 *
 * # Safety
 *
 * - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄。
 * - `method` 必须为 `NULL` 或调用期间可读的 `NUL` 结尾字符串。
 * - `params` 在 `params_len` 非零时必须指向调用期间可读的 `params_len` 字节；本函数返回后不再访问。
 * - `user_data` 由本库原样传回，必须可在任意线程使用，且在回调触发前保持有效。
 * - `out_call` 可以为 `NULL`（无需取消时）；若非空则必须对齐且独占可写，写入的句柄须通过 `zzu_call_free` 释放。
 */
ZzuStatus zzu_cas_call(const struct ZzuCasClient *cas,
                       const char *method,
                       const uint8_t *params,
                       size_t params_len,
                       ZzuCallback callback,
                       void *user_data,
                       struct ZzuCall **out_call);

/**
 * 同步调用 CAS 客户端方法，阻塞当前线程直至操作完成或达到 `timeoutMs` 时限。
 *
 * 供无法接收跨线程回调的语言或场景使用，不支持中途取消。严禁在 `ZzuCallback` 回调线程中调用
 * （直接返回 `ZZU_INVALID_INPUT`，避免阻塞后台工作线程造成死锁）。
 *
 * 返回 `ZZU_OK` 时 `*out_payload` 为结果 JSON，否则为错误 JSON。
 *
 * # Safety
 *
 * - `cas`、`method`、`params` 的要求同 `zzu_cas_call`。
 * - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_cas_call_blocking(const struct ZzuCasClient *cas,
                                const char *method,
                                const uint8_t *params,
                                size_t params_len,
                                struct ZzuBuffer **out_payload);

/**
 * 将学期课表渲染为 `iCalendar`（`.ics`）文本（当前线程同步完成，不发起网络请求，无需客户端）。
 *
 * `params` 为 UTF-8 JSON 对象 `{"timetable": Timetable, "options"?: CalendarExportOptions}`，
 * 其中 `timetable` 为 `timetable` 方法返回的课表快照，`options` 可省略。
 * 返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的 `.ics` 文本，否则为错误 JSON。
 *
 * # Safety
 *
 * - `params` 在 `params_len` 非零时必须指向调用期间可读的 `params_len` 字节。
 * - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_timetable_to_calendar(const uint8_t *params,
                                    size_t params_len,
                                    struct ZzuBuffer **out_payload);

/**
 * 使用 `ZzuCasClient` 创建本科教务客户端（本地完成，不发起网络请求）。
 *
 * 新句柄与 `cas` 共享认证会话、运行时和时限；创建后释放 `cas` 不影响它。
 *
 * # Safety
 *
 * - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
 * - `out_eas` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_eas_free` 释放。
 * - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_eas_new(const struct ZzuCasClient *cas,
                      struct ZzuEasClient **out_eas,
                      struct ZzuBuffer **out_error);

/**
 * 释放教务客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
 *
 * 在途调用不受影响，其回调仍会照常触发。
 *
 * # Safety
 *
 * `eas` 必须为 `NULL`，或由 `zzu_eas_new` 返回且尚未释放的句柄；释放后不得再使用。
 */
void zzu_eas_free(struct ZzuEasClient *eas);

/**
 * 异步调用教务方法（如 `"semesters"`），调用与回调规则同 `zzu_cas_call`。
 *
 * # Safety
 *
 * `eas` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
 */
ZzuStatus zzu_eas_call(const struct ZzuEasClient *eas,
                       const char *method,
                       const uint8_t *params,
                       size_t params_len,
                       ZzuCallback callback,
                       void *user_data,
                       struct ZzuCall **out_call);

/**
 * 同步调用教务方法，规则同 `zzu_cas_call_blocking`。
 *
 * # Safety
 *
 * `eas` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
 */
ZzuStatus zzu_eas_call_blocking(const struct ZzuEasClient *eas,
                                const char *method,
                                const uint8_t *params,
                                size_t params_len,
                                struct ZzuBuffer **out_payload);

/**
 * 校验宿舍房间号（格式为 `area-building--level-room`；当前线程同步完成，不发起网络请求，无需客户端）。
 *
 * `room` 为 UTF-8 房间号文本（无需 `NUL` 结尾）。返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的规范房间号，否则为错误 JSON。
 *
 * # Safety
 *
 * - `room` 在 `room_len` 非零时必须指向调用期间可读的 `room_len` 字节。
 * - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_room_id_parse(const uint8_t *room,
                            size_t room_len,
                            struct ZzuBuffer **out_payload);

/**
 * 拼接指定支付渠道的收银台 URL（当前线程同步完成，不发起网络请求，无需客户端）。
 *
 * `params` 为 UTF-8 JSON 对象 `{"order": ..., "channel": ...}`，其中 `order` 原样传入 `create_recharge_order` 的返回值。
 * 返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的 URL，否则为错误 JSON。
 *
 * # Safety
 *
 * - `params` 在 `params_len` 非零时必须指向调用期间可读的 `params_len` 字节。
 * - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_recharge_order_checkout_url_for(const uint8_t *params,
                                              size_t params_len,
                                              struct ZzuBuffer **out_payload);

/**
 * 使用 `ZzuCasClient` 创建校园卡与电控客户端（本地完成，不发起网络请求）。
 *
 * 新句柄与 `cas` 共享认证会话、运行时和时限；创建后释放 `cas` 不影响它。
 *
 * # Safety
 *
 * - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
 * - `out_ecard` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_ecard_free` 释放。
 * - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_ecard_new(const struct ZzuCasClient *cas,
                        struct ZzuEcardClient **out_ecard,
                        struct ZzuBuffer **out_error);

/**
 * 释放校园卡客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
 *
 * 在途调用不受影响，其回调仍会照常触发。
 *
 * # Safety
 *
 * `ecard` 必须为 `NULL`，或由 `zzu_ecard_new` 返回且尚未释放的句柄；释放后不得再使用。
 */
void zzu_ecard_free(struct ZzuEcardClient *ecard);

/**
 * 异步调用校园卡方法（如 `"balance"`），调用与回调规则同 `zzu_cas_call`。
 *
 * # Safety
 *
 * `ecard` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
 */
ZzuStatus zzu_ecard_call(const struct ZzuEcardClient *ecard,
                         const char *method,
                         const uint8_t *params,
                         size_t params_len,
                         ZzuCallback callback,
                         void *user_data,
                         struct ZzuCall **out_call);

/**
 * 同步调用校园卡方法，规则同 `zzu_cas_call_blocking`。
 *
 * # Safety
 *
 * `ecard` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
 */
ZzuStatus zzu_ecard_call_blocking(const struct ZzuEcardClient *ecard,
                                  const char *method,
                                  const uint8_t *params,
                                  size_t params_len,
                                  struct ZzuBuffer **out_payload);

/**
 * 异步探测当前网络下的校园网 Portal 地址与本机内网 IP，结果可用于 `zzu_portal_new`。
 *
 * `options` 为 UTF-8 JSON 对象，可选字段 `timeoutMs`（总时限，单位毫秒，默认 `15000`）；`options_len` 为 `0` 时使用默认配置（此时 `options` 可传 `NULL`）。
 * `options` 在提交前同步校验，若 JSON 非法则直接返回 `ZZU_INVALID_INPUT` 且不触发回调；其余调用与回调规则同 `zzu_cas_call`。
 *
 * # Safety
 *
 * - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄。
 * - `options` 在 `options_len` 非零时必须指向调用期间可读的 `options_len` 字节。
 * - `user_data` 与 `out_call` 的要求同 `zzu_cas_call`。
 */
ZzuStatus zzu_portal_discover(const struct ZzuRuntime *runtime,
                              const uint8_t *options,
                              size_t options_len,
                              ZzuCallback callback,
                              void *user_data,
                              struct ZzuCall **out_call);

/**
 * 同步探测校园网 Portal，`options` 同 `zzu_portal_discover`，其余规则同 `zzu_cas_call_blocking`。
 *
 * # Safety
 *
 * - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄。
 * - `options` 在 `options_len` 非零时必须指向调用期间可读的 `options_len` 字节。
 * - `out_payload` 的要求同 `zzu_cas_call_blocking`。
 */
ZzuStatus zzu_portal_discover_blocking(const struct ZzuRuntime *runtime,
                                       const uint8_t *options,
                                       size_t options_len,
                                       struct ZzuBuffer **out_payload);

/**
 * 创建校园网 Portal 认证客户端（本地完成，不发起网络请求）。
 *
 * `config` 为 UTF-8 JSON 对象：`baseUrl`、`userIp` 必填；`sourceAddress` 可选（`"user_ip"` 或
 * `"system"`，默认 `"user_ip"`）；`timeoutMs` 可选（默认 `15000` 毫秒）。
 *
 * # Safety
 *
 * - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
 * - `config` 在 `config_len` 非零时必须指向调用期间可读的 `config_len` 字节。
 * - `out_portal` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_portal_free` 释放。
 * - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_portal_new(const struct ZzuRuntime *runtime,
                         const uint8_t *config,
                         size_t config_len,
                         struct ZzuPortalClient **out_portal,
                         struct ZzuBuffer **out_error);

/**
 * 释放 Portal 客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
 *
 * 在途调用不受影响，其回调仍会照常触发。
 *
 * # Safety
 *
 * `portal` 必须为 `NULL`，或由 `zzu_portal_new` 返回且尚未释放的句柄；释放后不得再使用。
 */
void zzu_portal_free(struct ZzuPortalClient *portal);

/**
 * 异步调用 Portal 方法（如 `"login"`），调用与回调规则同 `zzu_cas_call`。
 *
 * # Safety
 *
 * `portal` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
 */
ZzuStatus zzu_portal_call(const struct ZzuPortalClient *portal,
                          const char *method,
                          const uint8_t *params,
                          size_t params_len,
                          ZzuCallback callback,
                          void *user_data,
                          struct ZzuCall **out_call);

/**
 * 同步调用 Portal 方法，规则同 `zzu_cas_call_blocking`。
 *
 * # Safety
 *
 * `portal` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
 */
ZzuStatus zzu_portal_call_blocking(const struct ZzuPortalClient *portal,
                                   const char *method,
                                   const uint8_t *params,
                                   size_t params_len,
                                   struct ZzuBuffer **out_payload);

/**
 * 创建校园网自助服务客户端（本地完成，不发起网络请求）。
 *
 * `config` 为 UTF-8 JSON 对象：`baseUrl` 必填，`timeoutMs` 可选（默认 `15000` 毫秒）。
 *
 * # Safety
 *
 * - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
 * - `config` 在 `config_len` 非零时必须指向调用期间可读的 `config_len` 字节。
 * - `out_service` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_self_service_free` 释放。
 * - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_self_service_new(const struct ZzuRuntime *runtime,
                               const uint8_t *config,
                               size_t config_len,
                               struct ZzuSelfServiceClient **out_service,
                               struct ZzuBuffer **out_error);

/**
 * 释放自助服务客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
 *
 * 在途调用不受影响，其回调仍会照常触发。
 *
 * # Safety
 *
 * `service` 必须为 `NULL`，或由 `zzu_self_service_new` 返回且尚未释放的句柄；释放后不得再使用。
 */
void zzu_self_service_free(struct ZzuSelfServiceClient *service);

/**
 * 异步调用自助服务方法（如 `"online_devices"`），调用与回调规则同 `zzu_cas_call`。
 *
 * # Safety
 *
 * `service` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
 */
ZzuStatus zzu_self_service_call(const struct ZzuSelfServiceClient *service,
                                const char *method,
                                const uint8_t *params,
                                size_t params_len,
                                ZzuCallback callback,
                                void *user_data,
                                struct ZzuCall **out_call);

/**
 * 同步调用自助服务方法，规则同 `zzu_cas_call_blocking`。
 *
 * # Safety
 *
 * `service` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
 */
ZzuStatus zzu_self_service_call_blocking(const struct ZzuSelfServiceClient *service,
                                         const char *method,
                                         const uint8_t *params,
                                         size_t params_len,
                                         struct ZzuBuffer **out_payload);

/**
 * 创建后台异步运行时，通常每个进程只需创建一个。
 *
 * `options` 为 UTF-8 JSON 对象，可选字段 `workerThreads`（正整数，默认 `2`）与
 * `rootCertificatesPem`（PEM 格式的根证书，提供时只信任它们，Android 上必填）；
 * `options_len` 为 `0` 时使用默认配置（此时 `options` 可传 `NULL`）。
 *
 * # Safety
 *
 * - `options` 在 `options_len` 非零时必须指向调用期间可读的 `options_len` 字节。
 * - `out_runtime` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_runtime_free` 释放。
 * - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
 */
ZzuStatus zzu_runtime_new(const uint8_t *options,
                          size_t options_len,
                          struct ZzuRuntime **out_runtime,
                          struct ZzuBuffer **out_error);

/**
 * 释放运行时句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
 *
 * 仍在使用该运行时的客户端与在途调用不受影响：运行时在它们全部结束后才关闭。
 *
 * # Safety
 *
 * `runtime` 必须为 `NULL`，或由 `zzu_runtime_new` 返回且尚未释放的句柄；释放后不得再使用。
 */
void zzu_runtime_free(struct ZzuRuntime *runtime);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* LIBZZU_H */
