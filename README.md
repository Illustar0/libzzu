# libzzu

对郑州大学相关系统的客户端封装，包含统一身份认证、本科教务、校园卡、宿舍电控与校园网。

仓库分为两部分：`crates/zzu` 是异步 Rust 库，`crates/libzzu` 将其导出为供其他语言绑定的 C ABI（头文件见 [`crates/libzzu/include/libzzu.h`](crates/libzzu/include/libzzu.h)，接口与协议文档见 [`crates/libzzu/ABI.md`](crates/libzzu/ABI.md)）。

## 构建

构建脚本基于 .NET（见 [`build/Build.cs`](build/Build.cs)）：

```sh
# 格式化检查、Clippy 与测试
dotnet run --project build -- Check

# 编译并打包指定平台的动态库、静态库与头文件（输出至 dist/）
dotnet run --project build -- Package --platform <target>
```

支持的 `<target>` 列表见 [`build/platforms.json`](build/platforms.json)。

## 许可

MIT OR Apache-2.0
