# zzu

项目由两个 crate 组成：

| Crate | 目录 | 职责 |
| --- | --- | --- |
| `zzu` | `crates/zzu` | 实现协议、会话与数据模型，并作为 Rust 项目使用的公共 API |
| `libzzu` | `crates/libzzu` | 基于 `zzu` 暴露 C ABI，生成动态库和静态库，供各语言 SDK 封装 |

依赖关系：`libzzu → zzu`。

## Rust 使用

在使用方的 `Cargo.toml` 中添加本地依赖，将路径替换为实际位置：

```toml
[dependencies]
zzu = { path = "/path/to/libzzu/crates/zzu" }
```

## C ABI 使用

```sh
cargo build -p libzzu --release
```

头文件为 `crates/libzzu/include/libzzu.h`。ABI 的句柄与 `zzu` 的客户端一一对应，
每种客户端有 `new`、`free`、`call`、`call_blocking` 四个函数，业务方法经 `call` 按方法名分发，
参数与结果均为 JSON：

```c
zzu_eas_new(cas, &eas, NULL);
zzu_eas_call(eas, "semesters", NULL, 0, on_complete, user_data, NULL);
```

设计取舍、线程模型、方法目录和各语言接入要点见 [`crates/libzzu/ABI.md`](crates/libzzu/ABI.md)。

构建产物位于 `target/release/`：

| 平台 | 动态库 | 静态库 |
| --- | --- | --- |
| macOS | `libzzu.dylib` | `libzzu.a` |
| Linux | `libzzu.so` | `libzzu.a` |
| Windows MSVC | `zzu.dll` | `zzu.lib` |

## 开发检查

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
