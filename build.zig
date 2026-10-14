const std = @import("std");

pub fn build(b: *std.Build) !void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const os = target.result.os.tag;

    const kak_debug = b.option(bool, "kak-debug", "Define KAK_DEBUG (runtime assertions, unit tests at startup); defaults to true in Debug mode") orelse (optimize == .debug);
    const version = b.option([]const u8, "version", "Version string; defaults to .version or `git describe`") orelse detectVersion(b);

    const mod = b.createModule(.{
        .target = target,
        .optimize = optimize,
        .link_libc = true,
        .link_libcpp = true,
    });

    var flags: std.ArrayList([]const u8) = .empty;
    try flags.appendSlice(b.allocator, &.{
        "-std=c++2b",
        "-Wall",
        "-Wextra",
        "-pedantic",
        "-Wno-unused-parameter",
        "-Wno-sign-compare",
        // Zig always compiles C++ with its bundled clang
        "-fsized-deallocation",
    });
    if (kak_debug) try flags.append(b.allocator, "-DKAK_DEBUG");

    switch (os) {
        .haiku => {
            mod.linkSystemLibrary("network", .{});
            mod.linkSystemLibrary("be", .{});
        },
        .illumos => mod.linkSystemLibrary("socket", .{}),
        // Windows support relies on Cygwin/MSYS2, which Zig can't target.
        .windows => std.debug.panic("native Windows is unsupported; build with Cygwin or MSYS2 via the Makefile", .{}),
        // OpenBSD can't query the running executable's path, so it must be
        // baked in. The install prefix isn't known while configuring, so it
        // has to be passed explicitly.
        .openbsd => {
            const bin_path = b.option([]const u8, "kak-bin-path", "Absolute path kak will be installed to (required on OpenBSD)") orelse
                std.debug.panic("-Dkak-bin-path is required when targeting OpenBSD", .{});
            try flags.append(b.allocator, b.fmt("-DKAK_BIN_PATH=\"{s}\"", .{bin_path}));
        },
        else => {},
    }

    mod.addCSourceFiles(.{
        .root = b.path("src"),
        .files = try collectSources(b, "src"),
        .flags = flags.items,
    });

    const version_cc = b.addWriteFiles().add(
        "version.cc",
        b.fmt("namespace Kakoune {{ const char *version = \"{s}\"; }}\n", .{version}),
    );
    mod.addCSourceFile(.{ .file = version_cc, .flags = flags.items });

    const exe = b.addExecutable(.{ .name = "kak", .root_module = mod });
    b.installArtifact(exe);

    // Runtime files, laid out like `make install` so the binary finds
    // ../share/kak relative to itself.
    b.installFile("share/kak/kakrc", "share/kak/kakrc");
    const install_rc = b.addInstallDirectory(.{ .source_dir = b.path("rc"), .install_dir = .prefix, .install_subdir = "share/kak/rc" });
    b.getInstallStep().dependOn(&install_rc.step);
    b.installDirectory(.{ .source_dir = b.path("colors"), .install_dir = .prefix, .install_subdir = "share/kak/colors" });
    b.installDirectory(.{ .source_dir = b.path("doc/pages"), .install_dir = .prefix, .install_subdir = "share/kak/doc" });
    b.installFile("README.asciidoc", "share/doc/kak/README.asciidoc");
    b.installFile("doc/kak.1", "share/man/man1/kak.1");
    if (kak_debug) b.installFile("gdb/kakoune.py", "share/kak/gdb/kakoune.py");
    const autoload = b.addSystemCommand(&.{ "sh", "-c", "ln -sfn rc \"$0/autoload\"" });
    autoload.addDirectoryArg(b.graph.path(.install_prefix, "share/kak"));
    autoload.has_side_effects = true;
    autoload.step.dependOn(&install_rc.step);
    b.getInstallStep().dependOn(&autoload.step);

    const run_step = b.step("run", "Run kak");
    const run_cmd = b.addRunArtifact(exe);
    run_cmd.step.dependOn(b.getInstallStep());
    run_cmd.addPassthruArgs();
    run_step.dependOn(&run_cmd.step);

    // Integration tests (test/run) against the installed binary.
    // Pass test directories after `--`, e.g. `zig build test -- test/normal`.
    const test_step = b.step("test", "Run the integration test suite");
    const test_cmd = b.addSystemCommand(&.{ "sh", "-c", "KAK=\"$(realpath \"$0\")\" exec sh test/run \"$@\"" });
    test_cmd.addFileArg(b.graph.path(.install_bin, "kak"));
    test_cmd.setCwd(b.path("."));
    test_cmd.has_side_effects = true;
    test_cmd.addPassthruArgs();
    test_cmd.step.dependOn(b.getInstallStep());
    test_step.dependOn(&test_cmd.step);
}

fn collectSources(b: *std.Build, sub_path: []const u8) ![]const []const u8 {
    const io = b.graph.io;
    var dir = try b.root.openDir(io, sub_path, .{ .iterate = true });
    defer dir.close(io);

    var files: std.ArrayList([]const u8) = .empty;
    var it = dir.iterate();
    while (try it.next(io)) |entry| {
        // Skip the Makefile's generated src/.version.cc
        if (entry.kind != .file or entry.name[0] == '.') continue;
        if (!std.mem.endsWith(u8, entry.name, ".cc")) continue;
        try files.append(b.allocator, b.dupe(entry.name));
    }
    std.mem.sort([]const u8, files.items, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.lessThan(u8, lhs, rhs);
        }
    }.lessThan);
    return files.items;
}

fn detectVersion(b: *std.Build) []const u8 {
    const io = b.graph.io;
    var buf: [256]u8 = undefined;
    if (b.root.root_dir.handle.readFile(io, ".version", &buf)) |contents| {
        return b.dupe(std.mem.trim(u8, contents, " \r\n"));
    } else |_| {}
    switch (b.runFallible(&.{ "git", "-C", b.root.joinString(b.allocator, "") catch ".", "describe", "--tags", "HEAD" }, .{ .stderr_behavior = .ignore })) {
        .success => |out| {
            const v = std.mem.trim(u8, out, " \r\n");
            return if (std.mem.startsWith(u8, v, "v")) v[1..] else v;
        },
        else => return "unknown",
    }
}
