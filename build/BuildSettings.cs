using System.Text.Json.Serialization;

sealed record BuildSettings(ToolVersions Tools, PlatformDefinition[] Platforms);

sealed record ToolVersions(string CargoNdk, string Ndk, int AndroidApi);

[JsonConverter(typeof(JsonStringEnumConverter<BuildBackend>))]
enum BuildBackend
{
    Cargo,
    Cross,
    Android
}

sealed record PlatformDefinition
{
    public required string Target { get; init; }
    public required string Runner { get; init; }
    public required BuildBackend Builder { get; init; }
    public bool StaticOnly { get; init; }
    public bool BuildStd { get; init; }
    public string? AndroidAbi { get; init; }
    public string? MsvcArch { get; init; }
    public Dictionary<string, string> Environment { get; init; } = [];
    [JsonPropertyName("rustflags")]
    public string[] RustFlags { get; init; } = [];
}
