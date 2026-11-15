using System.Formats.Tar;
using System.IO.Compression;
using System.Security.Cryptography;
using System.Text.Json;
using Fallout.Common;
using Fallout.Common.IO;
using Fallout.Common.Tooling;
using Serilog;

class Build : FalloutBuild
{
    public static int Main() => Execute<Build>(build => build.Check);

    [Parameter("Rust target triple to package")]
    readonly string Platform = "";

    [Parameter("Comma-separated Rust targets for the CI matrix; empty selects all")]
    readonly string Platforms = "";

    [PathVariable]
    readonly Tool Cargo = null!;

    [PathVariable]
    readonly Tool Rustup = null!;

    static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        WriteIndented = true
    };

    BuildSettings? settings;
    string? version;

    BuildSettings Settings => settings ??= JsonSerializer.Deserialize<BuildSettings>(
        File.ReadAllText(RootDirectory / "build/platforms.json"), JsonOptions)!;

    PlatformDefinition SelectedPlatform => Settings.Platforms.FirstOrDefault(platform => platform.Target == Platform)
        ?? throw new ArgumentException($"Unsupported Rust target: {Platform}");

    string Version
    {
        get
        {
            if (version is not null)
                return version;
            var output = Cargo("metadata --locked --no-deps --format-version 1", workingDirectory: RootDirectory, logOutput: false);
            using var metadata = JsonDocument.Parse(string.Join('\n', output.Where(line => line.Type == OutputType.Std).Select(line => line.Text)));
            version = metadata.RootElement.GetProperty("packages").EnumerateArray()
                .First(package => package.GetProperty("name").GetString() == "libzzu")
                .GetProperty("version").GetString()!;
            return version;
        }
    }

    AbsolutePath DistDirectory => RootDirectory / "dist";
    AbsolutePath CargoTargetDirectory => RootDirectory / "target/package-build" / Platform;
    AbsolutePath BuildDirectory => CargoTargetDirectory / Platform / "release";
    AbsolutePath PackageDirectory => DistDirectory / $"libzzu-{Version}-{Platform}";

    Target Format => target => target
        .Executes(() => Cargo("fmt --all -- --check", workingDirectory: RootDirectory, logger: LogToolOutput));

    Target Lint => target => target
        .After(Format)
        .Executes(() => Cargo("clippy --workspace --all-targets --locked -- -D warnings", workingDirectory: RootDirectory, logger: LogToolOutput));

    Target Test => target => target
        .After(Lint)
        .Executes(() => Cargo("test --workspace --all-targets --locked", workingDirectory: RootDirectory, logger: LogToolOutput));

    Target Check => target => target
        .DependsOn(Format, Lint, Test);

    Target Matrix => target => target
        .Executes(() =>
        {
            var platforms = string.IsNullOrWhiteSpace(Platforms)
                ? Settings.Platforms
                : Platforms.Split(',').Select(value => Settings.Platforms.First(platform => platform.Target == value.Trim())).ToArray();
            var matrix = JsonSerializer.Serialize(new
            {
                include = platforms.Select(platform => new
                {
                    target = platform.Target,
                    runner = platform.Runner
                })
            });
            Log.Information("{Matrix}", matrix);
            if (Environment.GetEnvironmentVariable("GITHUB_OUTPUT") is { } output)
                File.AppendAllText(output, $"matrix={matrix}{Environment.NewLine}");
        });

    Target Toolchain => target => target
        .Requires(() => Platform)
        .Executes(() =>
        {
            if (SelectedPlatform.BuildStd)
                Rustup("toolchain install nightly --profile minimal --component rust-src", workingDirectory: RootDirectory, logger: LogToolOutput);
            else
                Rustup($"target add {Platform}", workingDirectory: RootDirectory, logger: LogToolOutput);
            switch (SelectedPlatform.Builder)
            {
                case BuildBackend.Cross:
                    Cargo("install cross --git https://github.com/cross-rs/cross --locked", workingDirectory: RootDirectory, logger: LogToolOutput);
                    break;
                case BuildBackend.Android:
                    if (!Directory.Exists(AndroidNdkDirectory))
                        Run("sdkmanager", $"\"ndk;{Settings.Tools.Ndk}\"");
                    Cargo($"install cargo-ndk --locked --version {Settings.Tools.CargoNdk}", workingDirectory: RootDirectory, logger: LogToolOutput);
                    break;
            }
        });

    string AndroidNdkDirectory => Environment.GetEnvironmentVariable("ANDROID_NDK_HOME")
        ?? Path.Combine(Environment.GetEnvironmentVariable("ANDROID_HOME")
            ?? throw new InvalidOperationException("Set ANDROID_NDK_HOME or ANDROID_HOME"), "ndk", Settings.Tools.Ndk);

    Target Compile => target => target
        .DependsOn(Toolchain)
        .Executes(() =>
        {
            var platform = SelectedPlatform;
            var environment = EnvironmentInfo.Variables.ToDictionary(variable => variable.Key, variable => variable.Value);
            foreach (var (name, value) in platform.Environment)
                environment[name] = value;
            environment["CARGO_TARGET_DIR"] = CargoTargetDirectory;
            var inheritedFlags = Environment.GetEnvironmentVariable("CARGO_ENCODED_RUSTFLAGS")?.Split('\u001f')
                ?? (Environment.GetEnvironmentVariable("RUSTFLAGS") ?? "").Split(' ', StringSplitOptions.RemoveEmptyEntries);
            environment["CARGO_ENCODED_RUSTFLAGS"] = string.Join('\u001f', inheritedFlags.Concat(platform.RustFlags));
            var crateTypes = platform.StaticOnly ? "staticlib" : "cdylib,staticlib";
            var arguments = $"rustc --locked --release --package libzzu --lib --crate-type {crateTypes} --message-format=json-render-diagnostics";
            var tool = Cargo;
            if (platform.Builder == BuildBackend.Android)
            {
                environment["ANDROID_NDK_HOME"] = AndroidNdkDirectory;
                arguments = $"ndk --target {platform.AndroidAbi} --platform {Settings.Tools.AndroidApi} {arguments}";
            }
            else
            {
                arguments += $" --target {Platform}";
                if (platform.Builder == BuildBackend.Cross)
                    tool = ToolResolver.GetPathTool("cross");
            }
            if (platform.BuildStd)
            {
                environment["CROSS_BUILD_STD"] = "std,panic_unwind";
                arguments = $"+nightly {arguments}";
            }
            var output = tool(arguments, workingDirectory: RootDirectory, environmentVariables: environment,
                logger: (type, text) =>
                {
                    if (type == OutputType.Err)
                        LogToolOutput(type, text);
                });
            string? headerPath = null;
            foreach (var line in output.Where(line => line.Type == OutputType.Std && line.Text.StartsWith('{')))
            {
                using var message = JsonDocument.Parse(line.Text);
                if (message.RootElement.GetProperty("reason").GetString() != "build-script-executed")
                    continue;
                var outputDirectory = message.RootElement.GetProperty("out_dir").GetString()!;
                if (platform.Builder == BuildBackend.Cross)
                    outputDirectory = Path.Combine(CargoTargetDirectory, Path.GetRelativePath("/target", outputDirectory));
                var candidate = Path.Combine(outputDirectory, "libzzu.h");
                if (File.Exists(candidate))
                    headerPath = candidate;
            }
            // cargo-ndk 不转发 cargo 的 JSON 消息；入库头文件与生成结果的一致性由 cargo test 保证。
            if (platform.Builder == BuildBackend.Android)
                headerPath = RootDirectory / "crates/libzzu/include/libzzu.h";
            Directory.CreateDirectory(PackageDirectory / "include");
            File.Copy(headerPath ?? throw new InvalidOperationException("Cargo did not report generated libzzu.h"),
                PackageDirectory / "include/libzzu.h", true);
        });

    Target Package => target => target
        .DependsOn(Compile)
        .Produces(DistDirectory / "*.tar.gz*")
        .Executes(() =>
        {
            var platform = SelectedPlatform;
            string[] libraries = platform.StaticOnly ? ["libzzu.a"]
                : Platform.EndsWith("-windows-msvc", StringComparison.Ordinal) ? ["zzu.dll", "zzu.dll.lib", "zzu.lib"]
                : Platform.EndsWith("-windows-gnu", StringComparison.Ordinal) ? ["zzu.dll", "libzzu.dll.a", "libzzu.a"]
                : Platform.Contains("-apple-", StringComparison.Ordinal) ? ["libzzu.dylib", "libzzu.a"]
                : ["libzzu.so", "libzzu.a"];
            Directory.CreateDirectory(PackageDirectory / "lib");
            foreach (var library in libraries)
                File.Copy(BuildDirectory / library, PackageDirectory / "lib" / library, true);
            if (File.Exists(BuildDirectory / "zzu.pdb"))
                File.Copy(BuildDirectory / "zzu.pdb", PackageDirectory / "lib/zzu.pdb", true);
            File.WriteAllText(PackageDirectory / "manifest.json", JsonSerializer.Serialize(new
            {
                Version,
                Platform = platform,
                ConfiguredTools = Settings.Tools,
                Libraries = libraries
            }, JsonOptions));
            CreateArchive(PackageDirectory);
        });

    Target ExtractIosPackages => target => target
        .Executes(() =>
        {
            foreach (var archive in Directory.EnumerateFiles(DistDirectory, "libzzu-*-apple-ios*.tar.gz"))
            {
                using var file = File.OpenRead(archive);
                using var gzip = new GZipStream(file, CompressionMode.Decompress);
                TarFile.ExtractToDirectory(gzip, DistDirectory, overwriteFiles: true);
            }
        });

    Target Xcframework => target => target
        .DependsOn(ExtractIosPackages)
        .Produces(DistDirectory / "libzzu-*-ios.tar.gz*")
        .Executes(() =>
        {
            var device = DistDirectory / $"libzzu-{Version}-aarch64-apple-ios";
            var armSimulator = DistDirectory / $"libzzu-{Version}-aarch64-apple-ios-sim";
            var intelSimulator = DistDirectory / $"libzzu-{Version}-x86_64-apple-ios";
            var simulatorLibrary = TemporaryDirectory / "libzzu-ios-simulator.a";
            var directory = DistDirectory / $"libzzu-{Version}-ios";
            Directory.CreateDirectory(directory);
            Run("xcrun", $"lipo -create {armSimulator / "lib/libzzu.a"} {intelSimulator / "lib/libzzu.a"} -output {simulatorLibrary}");
            Run("xcodebuild", $"-create-xcframework -library {device / "lib/libzzu.a"} -headers {device / "include"} -library {simulatorLibrary} -headers {armSimulator / "include"} -output {directory / "libzzu.xcframework"}");
            CreateArchive(directory);
        });

    void Run(string tool, ArgumentStringHandler arguments) => ToolResolver.GetPathTool(tool)(arguments, workingDirectory: RootDirectory, logger: LogToolOutput);

    static void LogToolOutput(OutputType _, string output) => Log.Information("{Output}", output);

    static void CreateArchive(AbsolutePath directory)
    {
        var filename = $"{directory}.tar.gz";
        using (var file = File.Create(filename))
        using (var gzip = new GZipStream(file, CompressionLevel.SmallestSize))
            TarFile.CreateFromDirectory(directory, gzip, includeBaseDirectory: true);
        using var archive = File.OpenRead(filename);
        File.WriteAllText($"{filename}.sha256", $"{Convert.ToHexStringLower(SHA256.HashData(archive))}  {Path.GetFileName(filename)}\n");
        Log.Information("Package: {Archive}", filename);
    }
}
